//! The single [`ContractIndexer`]: one [`Indexer`] over a combined
//! multi-address/multi-topic filter, one cursor, one position-keyed store,
//! composed from per-domain [`DomainRegistration`]s.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use alloy_primitives::Address;
use alloy_rpc_types_eth::{Filter, Log};
use tracing::warn;
use vertex_chain_index::{CursorTables, IndexError, Indexer};
use vertex_storage::{Database, DatabaseError, DbTx, DbTxMut, Table, Tables};

use crate::reducer::Reducer;
use crate::registration::{DomainRegistration, RegistrationError, WatchedContract};
use crate::store::{EventKey, EventTable, events_of_tx, put_event, stored_event_from_log};
use crate::tag::ContractTag;

/// The indexer name: the engine's single cursor key and metric label.
pub const INDEXER_NAME: &str = "chain_contracts";

/// The one indexer that watches every registered Swarm contract.
///
/// Registered as a single [`Indexer`] with the
/// [`EventEngine`](vertex_chain_index::EventEngine). The combined filter is one
/// `eth_getLogs` per page covering all contracts; the engine delivers the page
/// in global `(block, log_index)` order, and [`apply`](Indexer::apply) files
/// each log into its own `(tag, block, log_index)` slot, so two contracts'
/// events in the same block never contend.
pub struct ContractIndexer<DB: Database> {
    db: Arc<DB>,
    contracts: Vec<WatchedContract>,
    by_address: HashMap<Address, WatchedContract>,
    reducers: HashMap<ContractTag, Box<dyn Reducer<DB>>>,
}

impl<DB: Database> ContractIndexer<DB> {
    /// Build the unified indexer, validating tag/address/table-name uniqueness
    /// and reducer-tag matching across registrations, then initialising the union
    /// of their tables plus the shared event and cursor tables. Composition
    /// collisions return [`RegistrationError`]; a storage fault during init
    /// propagates.
    pub fn from_registrations(
        db: Arc<DB>,
        regs: Vec<DomainRegistration<DB>>,
    ) -> Result<Self, RegistrationError> {
        let mut contracts: Vec<WatchedContract> = Vec::new();
        let mut reducers: HashMap<ContractTag, Box<dyn Reducer<DB>>> = HashMap::new();
        let mut by_address: HashMap<Address, WatchedContract> = HashMap::new();
        let mut tags: HashSet<ContractTag> = HashSet::new();
        let mut table_names: Vec<&'static str> = Vec::new();
        let mut seen_tables: HashSet<&'static str> = HashSet::new();

        for reg in regs {
            for c in &reg.contracts {
                if !tags.insert(c.tag) {
                    return Err(RegistrationError::DuplicateTag(c.tag));
                }
                if by_address.insert(c.address, *c).is_some() {
                    return Err(RegistrationError::DuplicateAddress(c.address));
                }
                contracts.push(*c);
            }

            // table `init` does not dedup, so a name collision would silently
            // share storage.
            for &name in reg.tables {
                if !seen_tables.insert(name) {
                    return Err(RegistrationError::DuplicateTableName(name));
                }
                table_names.push(name);
            }

            for reducer in reg.reducers {
                let tag = reducer.tag();
                if !tags.contains(&tag) {
                    return Err(RegistrationError::TagReducerMismatch(tag));
                }
                reducers.insert(tag, reducer);
            }
        }

        init_tables(db.as_ref(), &table_names).map_err(RegistrationError::TableInit)?;

        Ok(Self {
            db,
            contracts,
            by_address,
            reducers,
        })
    }

    /// Resolve a log address to its watched contract, or None (the filter is
    /// slightly over-broad).
    fn resolve(&self, address: Address) -> Option<WatchedContract> {
        self.by_address.get(&address).copied()
    }
}

/// Create every projection table plus the shared [`EventTable`] and the engine
/// cursor table in one write transaction.
fn init_tables<DB: Database>(db: &DB, domain_tables: &[&'static str]) -> Result<(), DatabaseError> {
    db.update(|tx| {
        tx.ensure_table(EventTable::NAME)?;
        for name in CursorTables::NAMES {
            tx.ensure_table(name)?;
        }
        for name in domain_tables {
            tx.ensure_table(name)?;
        }
        Ok(())
    })
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
        // Address is the authority; an unwatched address is skipped, never misfiled.
        let Some(contract) = self.resolve(log.address()) else {
            return Ok(());
        };

        // Verify topic0 is declared for this contract (two contracts can share a
        // topic0).
        let Some(topic0) = log.topic0().copied() else {
            // Anonymous event; no watched event is anonymous, so skip.
            return Ok(());
        };
        if !contract.declares(topic0) {
            return Ok(());
        }

        let log_index = log
            .log_index
            .ok_or(IndexError::MalformedLog { field: "log_index" })?;
        let key = EventKey {
            tag: contract.tag,
            block,
            log_index,
        };

        let event = stored_event_from_log(log);
        let data_len = event.data.len();
        let reducer = self.reducers.get(&contract.tag);

        // Reducer folds in the same tx as put_event; a decode miss is a skip, only
        // a storage fault escapes.
        self.db.update(|tx| {
            let stored = put_event(tx, key, &event)?;
            if !stored {
                warn!(
                    tag = contract.tag.0,
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
            let tag = contract.tag;
            let reducer = self.reducers.get(&tag).map(|r| r.as_ref());
            self.db
                .update(|tx| revert_contract::<DB>(tx, tag, reducer, from_block))?;
        }
        Ok(())
    }
}

/// Range-delete this contract's raw rows at or after `from_block`, then rebuild
/// its projection (if any) from the survivors.
///
/// Views derive purely from raw rows, so this is sufficient even for a batch
/// created earlier but mutated within the reverted range.
fn revert_contract<DB: Database>(
    tx: &DB::TXMut,
    tag: ContractTag,
    reducer: Option<&dyn Reducer<DB>>,
    from_block: u64,
) -> Result<(), DatabaseError> {
    let doomed: Vec<EventKey> = tx
        .range::<EventTable>(
            EventKey::range_from(tag, from_block),
            EventKey::range_end(tag),
        )?
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    for key in doomed {
        tx.delete::<EventTable>(key)?;
    }

    if let Some(reducer) = reducer {
        let surviving = events_of_tx(tx, tag)?;
        reducer.rebuild(tx, &surviving)?;
    }

    Ok(())
}
