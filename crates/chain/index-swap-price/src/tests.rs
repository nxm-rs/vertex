//! Unit tests over synthetic logs (no chain).
//!
//! These build canonical swap-price-oracle logs by ABI-encoding the two events,
//! fold them through [`SwapPriceIndexer::apply`], and assert the projection holds
//! the decoded value. They also assert the fold is idempotent (replay is a no-op)
//! and monotonic (a stale, reordered log never rolls the row back), and that an
//! unrelated topic is ignored.

use std::sync::Arc;

use alloy_primitives::{Address, B256, LogData, U256};
use alloy_rpc_types_eth::Log;
use alloy_sol_types::SolEvent;
use nectar_contracts::{ISwapPriceOracle, SwapPriceOracle};
use vertex_chain_index::Indexer;
use vertex_storage_redb::RedbDatabase;

use crate::indexer::SwapPriceIndexer;
use crate::projection::{LogPosition, SwapPriceField, read_field};

/// The contract address used in the synthetic logs.
const ORACLE: Address = Address::repeat_byte(0x5a);

/// A deployment fixture: the test oracle address, deployment block 0.
fn deployment() -> SwapPriceOracle {
    SwapPriceOracle::new(ORACLE, 0)
}

/// Build an indexer over a fresh in-memory database with its table created.
fn fresh_indexer() -> (SwapPriceIndexer<RedbDatabase>, Arc<RedbDatabase>) {
    let db = Arc::new(RedbDatabase::in_memory().expect("in-memory db"));
    let indexer = SwapPriceIndexer::new(deployment(), db.clone());
    indexer.init().expect("init projection table");
    (indexer, db)
}

/// A synthetic `PriceUpdate` log at `(block, index)` carrying `price`.
fn price_update_log(block: u64, index: u64, price: U256) -> Log {
    let event = ISwapPriceOracle::PriceUpdate { price };
    log_for(
        block,
        index,
        event.encode_topics_array::<1>()[0].into(),
        event,
    )
}

/// A synthetic `ChequeValueDeductionUpdate` log at `(block, index)`.
fn deduction_log(block: u64, index: u64, deduction: U256) -> Log {
    let event = ISwapPriceOracle::ChequeValueDeductionUpdate {
        chequeValueDeduction: deduction,
    };
    log_for(
        block,
        index,
        event.encode_topics_array::<1>()[0].into(),
        event,
    )
}

/// Wrap an encoded event into an RPC [`Log`] at a chosen position.
fn log_for<E: SolEvent>(block: u64, index: u64, topic0: B256, event: E) -> Log {
    let topics = vec![topic0];
    let data = event.encode_data().into();
    Log {
        inner: alloy_primitives::Log {
            address: ORACLE,
            data: LogData::new_unchecked(topics, data),
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

#[test]
fn filter_selects_both_event_topics() {
    let (indexer, _db) = fresh_indexer();
    let filter = indexer.filter();
    // The filter's topic0 set selects exactly the two indexed events.
    assert!(
        filter.topics[0].matches(&ISwapPriceOracle::PriceUpdate::SIGNATURE_HASH),
        "filter selects PriceUpdate"
    );
    assert!(
        filter.topics[0].matches(&ISwapPriceOracle::ChequeValueDeductionUpdate::SIGNATURE_HASH),
        "filter selects ChequeValueDeductionUpdate"
    );
}

#[test]
fn price_update_folds_into_exchange_rate() {
    let (indexer, db) = fresh_indexer();
    let rate = U256::from(123_456_789_u64);

    indexer
        .apply(40_000_000, &price_update_log(40_000_000, 2, rate))
        .expect("apply price update");

    let row = read_field(db.as_ref(), SwapPriceField::ExchangeRate)
        .expect("read")
        .expect("rate row present");
    assert_eq!(row.value, rate);
    assert_eq!(
        row.source,
        LogPosition {
            block: 40_000_000,
            log_index: 2
        }
    );
    // The deduction row is untouched.
    assert!(
        read_field(db.as_ref(), SwapPriceField::ChequeValueDeduction)
            .expect("read")
            .is_none()
    );
}

#[test]
fn deduction_update_folds_into_deduction_row() {
    let (indexer, db) = fresh_indexer();
    let deduction = U256::from(42_u64);

    indexer
        .apply(40_000_100, &deduction_log(40_000_100, 0, deduction))
        .expect("apply deduction update");

    let row = read_field(db.as_ref(), SwapPriceField::ChequeValueDeduction)
        .expect("read")
        .expect("deduction row present");
    assert_eq!(row.value, deduction);
    assert!(
        read_field(db.as_ref(), SwapPriceField::ExchangeRate)
            .expect("read")
            .is_none()
    );
}

#[test]
fn both_events_project_independently() {
    let (indexer, db) = fresh_indexer();
    let rate = U256::from(1_000_u64);
    let deduction = U256::from(7_u64);

    indexer.apply(100, &price_update_log(100, 0, rate)).unwrap();
    indexer
        .apply(101, &deduction_log(101, 0, deduction))
        .unwrap();

    assert_eq!(
        read_field(db.as_ref(), SwapPriceField::ExchangeRate)
            .unwrap()
            .unwrap()
            .value,
        rate
    );
    assert_eq!(
        read_field(db.as_ref(), SwapPriceField::ChequeValueDeduction)
            .unwrap()
            .unwrap()
            .value,
        deduction
    );
}

#[test]
fn reapplying_the_same_log_is_idempotent() {
    let (indexer, db) = fresh_indexer();
    let rate = U256::from(555_u64);
    let log = price_update_log(200, 3, rate);

    // Apply the identical finalized log twice: the second is a no-op replay.
    indexer.apply(200, &log).unwrap();
    let after_first = read_field(db.as_ref(), SwapPriceField::ExchangeRate)
        .unwrap()
        .unwrap();
    indexer.apply(200, &log).unwrap();
    let after_second = read_field(db.as_ref(), SwapPriceField::ExchangeRate)
        .unwrap()
        .unwrap();

    assert_eq!(after_first, after_second, "replay leaves the row unchanged");
}

#[test]
fn newer_log_supersedes_and_stale_log_is_ignored() {
    let (indexer, db) = fresh_indexer();
    let old = U256::from(10_u64);
    let new = U256::from(20_u64);

    // A later log wins.
    indexer.apply(300, &price_update_log(300, 0, old)).unwrap();
    indexer.apply(305, &price_update_log(305, 0, new)).unwrap();
    assert_eq!(
        read_field(db.as_ref(), SwapPriceField::ExchangeRate)
            .unwrap()
            .unwrap()
            .value,
        new
    );

    // Re-delivering the older log (a reorder/replay) must not roll the row back.
    indexer.apply(300, &price_update_log(300, 0, old)).unwrap();
    let row = read_field(db.as_ref(), SwapPriceField::ExchangeRate)
        .unwrap()
        .unwrap();
    assert_eq!(
        row.value, new,
        "stale log does not overwrite the newer value"
    );
    assert_eq!(
        row.source,
        LogPosition {
            block: 305,
            log_index: 0
        }
    );
}

#[test]
fn same_block_higher_log_index_supersedes() {
    let (indexer, db) = fresh_indexer();

    indexer
        .apply(400, &price_update_log(400, 0, U256::from(1_u64)))
        .unwrap();
    indexer
        .apply(400, &price_update_log(400, 5, U256::from(2_u64)))
        .unwrap();

    let row = read_field(db.as_ref(), SwapPriceField::ExchangeRate)
        .unwrap()
        .unwrap();
    assert_eq!(row.value, U256::from(2_u64));
    assert_eq!(row.source.log_index, 5);
}

#[test]
fn unrelated_topic_is_ignored() {
    let (indexer, db) = fresh_indexer();

    // A log whose topic0 matches neither event (here `OwnershipTransferred`'s
    // shape is irrelevant; any other topic must be a no-op).
    let other = Log {
        inner: alloy_primitives::Log {
            address: ORACLE,
            data: LogData::new_unchecked(vec![B256::repeat_byte(0xff)], Default::default()),
        },
        block_hash: Some(B256::repeat_byte(1)),
        block_number: Some(500),
        block_timestamp: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: Some(0),
        removed: false,
    };

    indexer
        .apply(500, &other)
        .expect("unrelated log is ignored");
    assert!(
        read_field(db.as_ref(), SwapPriceField::ExchangeRate)
            .unwrap()
            .is_none()
    );
    assert!(
        read_field(db.as_ref(), SwapPriceField::ChequeValueDeduction)
            .unwrap()
            .is_none()
    );
}
