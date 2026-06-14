//! The single [`ContractIndexer`]: ONE `impl Indexer` over a combined
//! multi-address / multi-topic filter, ONE cursor, ONE position-keyed store.
//!
//! `apply` keeps the #313 safety envelope: it resolves `log.address()` to a
//! [`ContractId`] (the address is the authority), verifies the `topic0` is
//! declared for that contract (skip otherwise), enforces the
//! [`MAX_EVENT_DATA`](crate::store::MAX_EVENT_DATA) cap, and writes the row
//! verbatim into [`EventTable`](crate::store::EventTable). It then dispatches the
//! row to the contract's [`Reducer`] (if any), which decodes and folds into its
//! typed projection(s) **in the same transaction**, so the raw row, the
//! projection, and the cursor commit atomically.
//!
//! A reducer's decode miss is a skip (`Ok(())`), never an error, so a malformed
//! body cannot wedge the shared cursor; the only `apply`-time errors are a
//! missing `log_index` (which a canonical finalized log never hits) and a genuine
//! storage fault. A contract with no reducer stays pure-lazy and pays nothing.
//!
//! `revert` is generic: per contract it range-deletes the raw tail at or after
//! `from_block`, then asks the contract's reducer to [`rebuild`](Reducer::rebuild)
//! its projection from the surviving raw rows. The verbatim store is the source
//! of truth underneath.

use std::collections::HashMap;
use std::sync::Arc;

use alloy_primitives::Address;
use alloy_rpc_types_eth::{Filter, Log};
use tracing::warn;
use vertex_chain_index::{IndexError, Indexer};
use vertex_storage::{Database, DbTxMut};

use crate::reducer::{PostageReducer, Reducer};
use crate::registry::{ContractId, Network, WatchedContract, registry};
use crate::store::{EventKey, EventTable, events_of_tx, put_event, stored_event_from_log};

/// The indexer name: the engine's single cursor key and metric label.
pub const INDEXER_NAME: &str = "chain_contracts";

/// The one indexer that watches every registered Swarm contract.
///
/// Registered as a single [`Indexer`] with the
/// [`EventEngine`](vertex_chain_index::EventEngine). The combined filter is one
/// `eth_getLogs` per page covering all contracts; the engine delivers the page
/// in global `(block, log_index)` order, and [`apply`](Indexer::apply) files
/// each log into its own `(contract, block, log_index)` slot, so two contracts'
/// events in the same block never contend.
pub struct ContractIndexer<DB> {
    db: Arc<DB>,
    contracts: Vec<WatchedContract>,
    by_address: HashMap<Address, WatchedContract>,
    reducers: HashMap<ContractId, Reducer>,
}

impl<DB: Database> ContractIndexer<DB> {
    /// Build the indexer for `network` from the canonical nectar address book,
    /// initializing the store's tables.
    pub fn new(db: Arc<DB>, network: Network) -> Result<Self, IndexError> {
        Self::with_contracts(db, registry(network))
    }

    /// Build the indexer over an explicit contract set.
    ///
    /// Production uses [`new`](Self::new) (the full registry); tests use this to
    /// watch a subset or to point a contract at a synthetic address.
    pub fn with_contracts(
        db: Arc<DB>,
        contracts: Vec<WatchedContract>,
    ) -> Result<Self, IndexError> {
        crate::store::ContractIndexTables::init(db.as_ref())?;
        let by_address = contracts.iter().map(|c| (c.address, *c)).collect();
        Ok(Self {
            db,
            contracts,
            by_address,
            reducers: Self::reducers(),
        })
    }

    /// The per-contract reducer set: one entry per contract that maintains an
    /// eager projection. Adding an eager contract is one entry here plus its
    /// [`Reducer`] arm. Contracts absent from this map stay verbatim-only.
    fn reducers() -> HashMap<ContractId, Reducer> {
        let mut m = HashMap::new();
        m.insert(ContractId::Postage, Reducer::Postage(PostageReducer));
        m
    }

    /// Resolve a log's address to its watched contract, if any.
    ///
    /// The address is the authority: a row is filed under the [`ContractId`] its
    /// emitting address maps to, never under a topic0 guess. A log from an
    /// address not in the registry (which the filter should exclude, but the
    /// address-set filter is slightly over-broad) resolves to `None` and is
    /// skipped.
    fn resolve(&self, address: Address) -> Option<WatchedContract> {
        self.by_address.get(&address).copied()
    }
}

impl<DB: Database> Indexer for ContractIndexer<DB> {
    fn name(&self) -> &'static str {
        INDEXER_NAME
    }

    fn start_block(&self) -> u64 {
        // The earliest deployment across the registry. The engine pages the
        // union range from here; a contract deployed later costs only empty
        // pages over the gap, which the adaptive paging covers cheaply.
        self.contracts
            .iter()
            .map(|c| c.start_block)
            .min()
            .unwrap_or(0)
    }

    fn filter(&self) -> Filter {
        let addresses: Vec<Address> = self.contracts.iter().map(|c| c.address).collect();
        let topics: Vec<_> = self
            .contracts
            .iter()
            .flat_map(|c| c.events.iter().map(|e| e.topic0))
            .collect();
        Filter::new().address(addresses).event_signature(topics)
    }

    fn apply(&self, block: u64, log: &Log) -> Result<(), IndexError> {
        // Address is the authority. A log from an unwatched address (the filter
        // is slightly over-broad) is skipped, never misfiled.
        let Some(contract) = self.resolve(log.address()) else {
            return Ok(());
        };

        // Topic-confusion defence: verify the topic0 is one this contract
        // declares. Two contracts can share a topic0 (postage and swap both emit
        // `PriceUpdate`); the address already disambiguated, and this rejects a
        // topic0 that belongs to a *different* watched contract at this address.
        let Some(topic0) = log.topic0().copied() else {
            // No topic0 is an anonymous event; none of the watched events are
            // anonymous, so skip it rather than file it under topic0 default.
            return Ok(());
        };
        if !contract.declares(topic0) {
            return Ok(());
        }

        let log_index = log
            .log_index
            .ok_or(IndexError::MalformedLog { field: "log_index" })?;
        let key = EventKey {
            contract: contract.id,
            block,
            log_index,
        };

        let event = stored_event_from_log(log);
        let data_len = event.data.len();

        // The contract's reducer (if any) decodes and folds into its typed
        // projection in the SAME tx as the verbatim put_event, so raw row +
        // projection + cursor commit atomically. A reducer decode miss is a skip
        // (Ok), so a malformed body never wedges the cursor; only a storage fault
        // escapes.
        let reducer = self.reducers.get(&contract.id);

        self.db.update(|tx| {
            let stored = put_event(tx, key, event.clone())?;
            if !stored {
                warn!(
                    contract = <&'static str>::from(contract.id),
                    block, log_index, data_len, "event data exceeds MAX_EVENT_DATA cap, skipping"
                );
                return Ok(());
            }
            if let Some(reducer) = reducer {
                reducer.reduce(tx, key, &event)?;
            }
            Ok(())
        })?;

        Ok(())
    }

    fn revert(&self, from_block: u64) -> Result<(), IndexError> {
        for contract in &self.contracts {
            let id = contract.id;
            let reducer = self.reducers.get(&id);
            self.db
                .update(|tx| revert_contract(tx, id, reducer, from_block))?;
        }
        Ok(())
    }
}

/// Revert one contract: range-delete its raw tail at or after `from_block`, then
/// rebuild its projection (if it has a reducer) from the surviving raw rows.
///
/// Because every view derives purely from the raw rows, deleting the reorged-out
/// range plus rebuilding the projection from survivors is necessary and
/// sufficient: no view holds independent state revert could miss. A batch created
/// BEFORE `from_block` but mutated within the reverted range is handled by the
/// reducer's full rebuild from survivors (it cannot be caught by a create-block
/// filter). The MVP engine indexes finalized-only and never calls this; it is
/// correct-by-construction today and correct-by-design when head-tracking arrives.
fn revert_contract<TX: DbTxMut>(
    tx: &TX,
    contract: ContractId,
    reducer: Option<&Reducer>,
    from_block: u64,
) -> Result<(), vertex_storage::DatabaseError> {
    // Drop this contract's events at or after from_block, found by a bounded
    // range scan over just this contract's tail range.
    let doomed: Vec<EventKey> = tx
        .range::<EventTable>(
            EventKey::range_from(contract, from_block),
            EventKey::range_end(contract),
        )?
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    for key in doomed {
        tx.delete::<EventTable>(key)?;
    }

    // Rebuild the projection (if any) from exactly the surviving rows (already
    // truncated to `< from_block`), so it can never drift from the source of
    // truth. A reducer-less contract has no projection and nothing to rebuild.
    if let Some(reducer) = reducer {
        let surviving = events_of_tx(tx, contract)?;
        reducer.rebuild(tx, &surviving)?;
    }

    Ok(())
}
