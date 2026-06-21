//! Mechanism tests over a synthetic registration (a dummy reducer over two
//! synthetic contracts), exercising store/apply/revert/idempotency/topic-confusion/
//! decode-miss without naming a real contract.

use std::sync::Arc;

use alloy_primitives::{Address, B256, Bytes, LogData, address};
use alloy_rpc_types_eth::Log;
use vertex_chain_index::Indexer;
use vertex_storage::{Database, DatabaseError, DbTx, DbTxMut, Table};
use vertex_storage_redb::RedbDatabase;

use crate::reducer::Reducer;
use crate::registration::{
    DomainRegistration, EventDescriptor, RegistrationError, WatchedContract,
};
use crate::store::{EventKey, EventTable, MAX_EVENT_DATA, StoredEvent};
use crate::tag::ContractTag;
use crate::{ContractIndexer, point_get};

// Two synthetic contracts: one with a reducer (FOO), one verbatim-only (BAR).
const TAG_FOO: ContractTag = ContractTag(0xF0);
const TAG_BAR: ContractTag = ContractTag(0xBA);
const FOO_ADDR: Address = address!("0000000000000000000000000000000000000f00");
const BAR_ADDR: Address = address!("0000000000000000000000000000000000000ba0");

// A synthetic topic0 each contract declares. FOO_TOPIC drives the dummy reducer;
// BAR_TOPIC is verbatim-only. Arbitrary 32-byte constants, not real event
// signatures (the framework names no ABI).
const FOO_TOPIC: B256 = B256::repeat_byte(0x11);
const BAR_TOPIC: B256 = B256::repeat_byte(0x22);

const FOO_EVENTS: &[EventDescriptor] = &[EventDescriptor {
    topic0: FOO_TOPIC,
    name: "Foo",
}];
const BAR_EVENTS: &[EventDescriptor] = &[EventDescriptor {
    topic0: BAR_TOPIC,
    name: "Bar",
}];

// A one-row projection the dummy reducer maintains, keyed by the contract tag so
// there is exactly one row per FOO contract.
crate::projection!(DummyTable, "test__dummy", DummyKey, u64);

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
struct DummyKey(u8);
vertex_storage::impl_fixed_codec!(DummyKey, 1);
impl From<DummyKey> for [u8; 1] {
    fn from(k: DummyKey) -> Self {
        [k.0]
    }
}
impl From<[u8; 1]> for DummyKey {
    fn from(b: [u8; 1]) -> Self {
        Self(b[0])
    }
}

/// The dummy reducer: on a FOO event whose data is exactly 8 bytes, store that
/// `u64`; any other body (too short / too long) is a decode-miss skip, never an
/// error. This stands in for a domain's typed `sol!` decode without an ABI.
struct DummyReducer;

impl DummyReducer {
    fn decode(ev: &StoredEvent) -> Option<u64> {
        let bytes: [u8; 8] = ev.data.as_ref().try_into().ok()?;
        Some(u64::from_be_bytes(bytes))
    }
}

impl<DB: Database> Reducer<DB> for DummyReducer {
    fn tag(&self) -> ContractTag {
        TAG_FOO
    }

    fn reduce(&self, tx: &DB::TXMut, key: EventKey, ev: &StoredEvent) -> Result<(), DatabaseError> {
        let Some(v) = Self::decode(ev) else {
            return Ok(()); // decode miss is a skip, never an error
        };
        tx.put::<DummyTable>(DummyKey(key.tag.0), v)
    }

    fn rebuild(
        &self,
        tx: &DB::TXMut,
        surviving: &[(EventKey, StoredEvent)],
    ) -> Result<(), DatabaseError> {
        tx.clear::<DummyTable>()?;
        for (key, ev) in surviving {
            if let Some(v) = Self::decode(ev) {
                tx.put::<DummyTable>(DummyKey(key.tag.0), v)?;
            }
        }
        Ok(())
    }
}

fn dummy_registration<DB: Database>() -> DomainRegistration<DB> {
    DomainRegistration {
        contracts: vec![
            WatchedContract {
                tag: TAG_FOO,
                address: FOO_ADDR,
                start_block: 0,
                events: FOO_EVENTS,
            },
            WatchedContract {
                tag: TAG_BAR,
                address: BAR_ADDR,
                start_block: 0,
                events: BAR_EVENTS,
            },
        ],
        reducers: vec![Box::new(DummyReducer)],
        tables: &[DummyTable::NAME],
    }
}

/// Build a synthetic log at `(block, index)` from `address` with `topic0` and a
/// raw `data` body.
fn log_with(block: u64, index: u64, address: Address, topic0: B256, data: Bytes) -> Log {
    Log {
        inner: alloy_primitives::Log {
            address,
            data: LogData::new_unchecked(vec![topic0], data),
        },
        block_hash: Some(B256::repeat_byte(block as u8)),
        block_number: Some(block),
        block_timestamp: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: Some(index),
        removed: false,
    }
}

fn harness() -> (Arc<RedbDatabase>, ContractIndexer<RedbDatabase>) {
    let db = Arc::new(RedbDatabase::in_memory().expect("in-memory db"));
    let indexer = ContractIndexer::from_registrations(db.clone(), vec![dummy_registration()])
        .expect("indexer");
    (db, indexer)
}

fn u64_body(v: u64) -> Bytes {
    Bytes::from(v.to_be_bytes().to_vec())
}

/// Extract the [`RegistrationError`] from a failed composition. `ContractIndexer`
/// is intentionally not `Debug` (it holds `Box<dyn Reducer>`), so the tests can't
/// use `unwrap_err`; this asserts the `Err` and returns it.
fn expect_err(
    result: Result<ContractIndexer<RedbDatabase>, RegistrationError>,
) -> RegistrationError {
    match result {
        Err(e) => e,
        Ok(_) => panic!("expected a RegistrationError"),
    }
}

#[test]
fn filter_unions_addresses_and_topics() {
    let (_db, indexer) = harness();
    let filter = indexer.filter();
    assert_eq!(filter.address.len(), 2);
    let topics = filter.topics[0].to_value_or_array();
    assert!(topics.is_some());
}

#[test]
fn unwatched_address_is_skipped() {
    let (db, indexer) = harness();
    let stray = address!("00000000000000000000000000000000deadbeef");
    let log = log_with(10, 0, stray, FOO_TOPIC, u64_body(7));
    indexer.apply(10, &log).expect("apply");
    let count = db.view(|tx| tx.count::<EventTable>()).unwrap();
    assert_eq!(
        count, 0,
        "a log from an unwatched address must not be stored"
    );
}

#[test]
fn topic_not_declared_for_resolved_contract_is_skipped() {
    // Emit BAR's topic0 at FOO's address. The address resolves to FOO, which does
    // NOT declare BAR_TOPIC, so the row must be skipped (never misfiled).
    let (db, indexer) = harness();
    let log = log_with(5, 0, FOO_ADDR, BAR_TOPIC, u64_body(1));
    indexer.apply(5, &log).expect("apply");
    let stored = db
        .view(|tx| {
            tx.get::<EventTable>(EventKey {
                tag: TAG_FOO,
                block: 5,
                log_index: 0,
            })
        })
        .unwrap();
    assert!(
        stored.is_none(),
        "a topic0 not declared for the resolved contract must be skipped"
    );
}

#[test]
fn oversized_data_is_capped() {
    let (db, indexer) = harness();
    let huge = Bytes::from(vec![0u8; MAX_EVENT_DATA + 1]);
    let log = log_with(9, 0, FOO_ADDR, FOO_TOPIC, huge);
    indexer
        .apply(9, &log)
        .expect("apply must not error on oversized data");
    let stored = db
        .view(|tx| {
            tx.get::<EventTable>(EventKey {
                tag: TAG_FOO,
                block: 9,
                log_index: 0,
            })
        })
        .unwrap();
    assert!(
        stored.is_none(),
        "data over MAX_EVENT_DATA must be skipped, not stored"
    );
}

#[test]
fn missing_log_index_errors() {
    let (_db, indexer) = harness();
    let mut log = log_with(3, 0, FOO_ADDR, FOO_TOPIC, u64_body(1));
    log.log_index = None;
    let err = indexer.apply(3, &log).unwrap_err();
    assert!(matches!(
        err,
        vertex_chain_index::IndexError::MalformedLog { field: "log_index" }
    ));
}

#[test]
fn replayed_log_is_idempotent() {
    let (db, indexer) = harness();
    let log = log_with(100, 0, FOO_ADDR, FOO_TOPIC, u64_body(42));
    indexer.apply(100, &log).expect("apply");
    indexer.apply(100, &log).expect("replay");
    let count = db.view(|tx| tx.count::<EventTable>()).unwrap();
    assert_eq!(
        count, 1,
        "a replayed (block, log_index) overwrites in place"
    );
}

#[test]
fn reducer_folds_into_projection() {
    let (db, indexer) = harness();
    let log = log_with(1, 0, FOO_ADDR, FOO_TOPIC, u64_body(99));
    indexer.apply(1, &log).expect("apply");
    let row = point_get::<DummyTable, _>(&*db, DummyKey(TAG_FOO.0)).unwrap();
    assert_eq!(row, Some(99));
}

#[test]
fn verbatim_only_contract_stores_without_projection() {
    let (db, indexer) = harness();
    // BAR has no reducer: its event is stored verbatim, no projection touched.
    indexer
        .apply(7, &log_with(7, 0, BAR_ADDR, BAR_TOPIC, u64_body(3)))
        .expect("apply");
    let stored = db
        .view(|tx| {
            tx.get::<EventTable>(EventKey {
                tag: TAG_BAR,
                block: 7,
                log_index: 0,
            })
        })
        .unwrap();
    assert!(
        stored.is_some(),
        "a verbatim-only contract still stores the row"
    );
    assert_eq!(
        point_get::<DummyTable, _>(&*db, DummyKey(TAG_BAR.0)).unwrap(),
        None
    );
}

#[test]
fn malformed_reducer_body_skips_not_wedges() {
    // A declared FOO topic0 with a body the dummy reducer cannot decode must:
    //  (1) NOT error apply (the cursor advances),
    //  (2) still land the verbatim row,
    //  (3) but produce NO projection row (the reducer decode miss is a skip).
    let (db, indexer) = harness();
    let garbage = Bytes::from(vec![0xFFu8; 3]); // not 8 bytes => decode miss
    let log = log_with(12, 0, FOO_ADDR, FOO_TOPIC, garbage);
    indexer
        .apply(12, &log)
        .expect("a malformed reducer body must not error apply");

    let stored = db
        .view(|tx| {
            tx.get::<EventTable>(EventKey {
                tag: TAG_FOO,
                block: 12,
                log_index: 0,
            })
        })
        .unwrap();
    assert!(stored.is_some(), "the verbatim row must still be stored");

    let row = point_get::<DummyTable, _>(&*db, DummyKey(TAG_FOO.0)).unwrap();
    assert!(
        row.is_none(),
        "a decode miss must produce no projection row"
    );
}

#[test]
fn revert_range_deletes_per_contract_and_rebuilds() {
    let (db, indexer) = harness();
    // Two FOO events; the later one wins the projection.
    indexer
        .apply(100, &log_with(100, 0, FOO_ADDR, FOO_TOPIC, u64_body(1)))
        .expect("apply");
    indexer
        .apply(200, &log_with(200, 0, FOO_ADDR, FOO_TOPIC, u64_body(2)))
        .expect("apply");
    assert_eq!(
        point_get::<DummyTable, _>(&*db, DummyKey(TAG_FOO.0)).unwrap(),
        Some(2)
    );

    // Revert from 150: the block-200 event is dropped, the projection rebuilds to
    // the surviving block-100 value.
    indexer.revert(150).expect("revert");
    let count = db.view(|tx| tx.count::<EventTable>()).unwrap();
    assert_eq!(count, 1, "the block-200 row must be range-deleted");
    assert_eq!(
        point_get::<DummyTable, _>(&*db, DummyKey(TAG_FOO.0)).unwrap(),
        Some(1),
        "the projection must rebuild from the surviving row"
    );
}

#[test]
fn from_registrations_rejects_duplicate_tag() {
    let db = Arc::new(RedbDatabase::in_memory().expect("db"));
    let dup = DomainRegistration::<RedbDatabase> {
        contracts: vec![
            WatchedContract {
                tag: TAG_FOO,
                address: FOO_ADDR,
                start_block: 0,
                events: FOO_EVENTS,
            },
            WatchedContract {
                tag: TAG_FOO, // duplicate tag
                address: BAR_ADDR,
                start_block: 0,
                events: BAR_EVENTS,
            },
        ],
        reducers: vec![],
        tables: &[],
    };
    let err = expect_err(ContractIndexer::from_registrations(db, vec![dup]));
    assert!(matches!(
        err,
        RegistrationError::DuplicateTag(ContractTag(0xF0))
    ));
}

#[test]
fn from_registrations_rejects_duplicate_address() {
    let db = Arc::new(RedbDatabase::in_memory().expect("db"));
    let dup = DomainRegistration::<RedbDatabase> {
        contracts: vec![
            WatchedContract {
                tag: TAG_FOO,
                address: FOO_ADDR,
                start_block: 0,
                events: FOO_EVENTS,
            },
            WatchedContract {
                tag: TAG_BAR,
                address: FOO_ADDR, // duplicate address
                start_block: 0,
                events: BAR_EVENTS,
            },
        ],
        reducers: vec![],
        tables: &[],
    };
    let err = expect_err(ContractIndexer::from_registrations(db, vec![dup]));
    assert!(matches!(err, RegistrationError::DuplicateAddress(_)));
}

#[test]
fn from_registrations_rejects_duplicate_table_name() {
    let db = Arc::new(RedbDatabase::in_memory().expect("db"));
    let a = DomainRegistration::<RedbDatabase> {
        contracts: vec![WatchedContract {
            tag: TAG_FOO,
            address: FOO_ADDR,
            start_block: 0,
            events: FOO_EVENTS,
        }],
        reducers: vec![],
        tables: &["shared__name"],
    };
    let b = DomainRegistration::<RedbDatabase> {
        contracts: vec![WatchedContract {
            tag: TAG_BAR,
            address: BAR_ADDR,
            start_block: 0,
            events: BAR_EVENTS,
        }],
        reducers: vec![],
        tables: &["shared__name"], // collides with `a`
    };
    let err = expect_err(ContractIndexer::from_registrations(db, vec![a, b]));
    assert!(matches!(
        err,
        RegistrationError::DuplicateTableName("shared__name")
    ));
}

#[test]
fn from_registrations_rejects_orphan_reducer() {
    let db = Arc::new(RedbDatabase::in_memory().expect("db"));
    let reg = DomainRegistration::<RedbDatabase> {
        contracts: vec![WatchedContract {
            tag: TAG_BAR, // only BAR is watched
            address: BAR_ADDR,
            start_block: 0,
            events: BAR_EVENTS,
        }],
        reducers: vec![Box::new(DummyReducer)], // DummyReducer.tag() == TAG_FOO
        tables: &[DummyTable::NAME],
    };
    let err = expect_err(ContractIndexer::from_registrations(db, vec![reg]));
    assert!(matches!(
        err,
        RegistrationError::TagReducerMismatch(ContractTag(0xF0))
    ));
}
