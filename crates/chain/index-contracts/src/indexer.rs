//! The single [`ContractIndexer`]: ONE `impl Indexer` over a combined
//! multi-address / multi-topic filter, ONE cursor, ONE position-keyed store.
//!
//! `apply` is a TOTAL function over the filter's result set. It never decodes an
//! event body. It resolves `log.address()` to a [`ContractId`] (the address is
//! the authority), verifies the `topic0` is declared for that contract (skip
//! otherwise), enforces the [`MAX_EVENT_DATA`](crate::store::MAX_EVENT_DATA)
//! cap, and writes the row verbatim into [`EventTable`](crate::store::EventTable).
//! For `Postage` rows it additionally feeds the typed
//! [`BatchTable`](crate::store::BatchTable) projection that backs the one
//! value-sorted index.
//!
//! Because the body is never parsed, a malformed or unexpected log cannot panic
//! or error the hot path; the only `apply`-time error is a missing `log_index`,
//! which a canonical finalized log never hits. Decoding is a
//! [`views`](crate::views) concern, scoped to one read.

use std::collections::HashMap;
use std::sync::Arc;

use alloy_primitives::Address;
use alloy_rpc_types_eth::{Filter, Log};
use alloy_sol_types::SolEvent;
use tracing::warn;
use vertex_chain_index::{IndexError, Indexer};
use vertex_storage::Database;

use crate::registry::{ContractId, Network, WatchedContract, abi, registry};
use crate::store::{
    BatchUpdate, ContractIndexTables, EventKey, apply_batch_update, put_event, revert_contract,
    stored_event_from_log,
};

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
        ContractIndexTables::init(db.as_ref())?;
        let by_address = contracts.iter().map(|c| (c.address, *c)).collect();
        Ok(Self {
            db,
            contracts,
            by_address,
        })
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

        // For Postage create/topup/depth-increase, decode the batch fields to
        // feed the typed projection that backs the value-sorted index. This is
        // the ONE place apply decodes, and only to maintain the eviction HINT;
        // the verbatim row is still the source of truth, and a decode failure
        // here is non-fatal (logged, the verbatim row still lands).
        let batch_update = if contract.id == ContractId::Postage {
            decode_batch_update(log, block, topic0)
        } else {
            None
        };

        self.db.update(|tx| {
            let stored = put_event(tx, key, event.clone())?;
            if !stored {
                warn!(
                    contract = <&'static str>::from(contract.id),
                    block,
                    log_index,
                    data_len = event.data.len(),
                    "event data exceeds MAX_EVENT_DATA cap, skipping"
                );
                return Ok(());
            }
            if let Some(update) = batch_update {
                apply_batch_update(tx, update)?;
            }
            Ok(())
        })?;

        Ok(())
    }

    fn revert(&self, from_block: u64) -> Result<(), IndexError> {
        for contract in &self.contracts {
            revert_contract(self.db.as_ref(), contract.id, from_block)?;
        }
        Ok(())
    }
}

/// Decode a postage batch lifecycle event into the typed projection update that
/// maintains the value-sorted index. Returns `None` for non-batch events and on
/// a decode miss (the verbatim row still lands; the index is a self-healing
/// hint, not the source of truth).
///
/// `BatchCreated` opens the row; `BatchTopUp` / `BatchDepthIncrease` raise the
/// `normalisedBalance` on the existing row so the value-sorted index moves with
/// the batch (the "eager re-position on topup/depth-increase as a local
/// optimization, never a generic callback" the reactions design blesses). The
/// reserve recomputes truth at dequeue regardless, so a stale hint only costs a
/// skip-and-reinsert.
fn decode_batch_update(
    log: &Log,
    block: u64,
    topic0: alloy_primitives::B256,
) -> Option<BatchUpdate> {
    if topic0 == abi::BatchCreated::SIGNATURE_HASH {
        let e = log.log_decode::<abi::BatchCreated>().ok()?.inner.data;
        Some(BatchUpdate::Created {
            batch_id: e.batchId,
            owner: e.owner,
            depth: e.depth,
            bucket_depth: e.bucketDepth,
            normalised_balance: e.normalisedBalance,
            immutable: e.immutableFlag,
            start_block: block,
        })
    } else if topic0 == abi::BatchTopUp::SIGNATURE_HASH {
        let e = log.log_decode::<abi::BatchTopUp>().ok()?.inner.data;
        Some(BatchUpdate::Balance {
            batch_id: e.batchId,
            normalised_balance: e.normalisedBalance,
        })
    } else if topic0 == abi::BatchDepthIncrease::SIGNATURE_HASH {
        let e = log.log_decode::<abi::BatchDepthIncrease>().ok()?.inner.data;
        Some(BatchUpdate::Depth {
            batch_id: e.batchId,
            new_depth: e.newDepth,
            normalised_balance: e.normalisedBalance,
        })
    } else {
        None
    }
}
