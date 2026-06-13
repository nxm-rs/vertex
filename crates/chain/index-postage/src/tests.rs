//! Unit tests over synthetic logs: each event decodes and folds into the
//! projection correctly, re-applying a log is idempotent, the supersede guard is
//! monotonic by `(block, log_index)`, and the validity query matches the
//! contract's arithmetic.
//!
//! No chain here. We build a [`Log`] for each event by ABI-encoding it through
//! the same `sol!` bindings the indexer decodes with, drive
//! [`Indexer::apply`](vertex_chain_index::Indexer::apply) directly, and read the
//! projection back from an in-memory `vertex-storage` backend.

use alloy_primitives::{Address, B256, U256, address};
use alloy_rpc_types_eth::Log;
use alloy_sol_types::SolEvent;
use vertex_chain_index::Indexer;

use crate::PostageIndexer;
use crate::events::{BatchCreated, BatchDepthIncrease, BatchTopUp, Paused, PriceUpdate};
use crate::indexer::POSTAGE_STAMP_ADDRESS;
use crate::projection::{ChainState, is_batch_valid_now, read_batch, read_chain_state};

type Db = vertex_storage_redb::RedbDatabase;

const BATCH: B256 = B256::repeat_byte(0xab);
const OWNER: Address = address!("00000000000000000000000000000000000000bb");

/// Build a `Log` from an ABI-encoded event, placed at `(block, index)` on the
/// PostageStamp contract address.
fn log_for<E: SolEvent>(event: &E, block: u64, index: u64) -> Log {
    let data = event.encode_log_data();
    Log {
        inner: alloy_primitives::Log {
            address: POSTAGE_STAMP_ADDRESS,
            data,
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

fn new_indexer() -> (PostageIndexer<Db>, std::sync::Arc<Db>) {
    let db = std::sync::Arc::new(Db::in_memory().expect("in-memory db"));
    let indexer = PostageIndexer::new(db.clone()).expect("init indexer");
    (indexer, db)
}

fn created(normalised: u64, depth: u8) -> BatchCreated {
    BatchCreated {
        batchId: BATCH,
        totalAmount: U256::from(1_000u64),
        normalisedBalance: U256::from(normalised),
        owner: OWNER,
        depth,
        bucketDepth: 16,
        immutableFlag: false,
    }
}

#[test]
fn batch_created_folds_into_batch_set() {
    let (indexer, db) = new_indexer();
    let log = log_for(&created(500, 20), 100, 0);
    indexer.apply(100, &log).expect("apply");

    let batch = read_batch(db.as_ref(), BATCH).unwrap().expect("batch row");
    assert_eq!(batch.owner, OWNER);
    assert_eq!(batch.depth, 20);
    assert_eq!(batch.bucket_depth, 16);
    assert_eq!(batch.normalised_balance, U256::from(500u64));
    assert!(!batch.immutable);
    assert_eq!(batch.start_block, 100);
}

#[test]
fn batch_topup_raises_normalised_balance_keeps_start_block() {
    let (indexer, db) = new_indexer();
    indexer
        .apply(100, &log_for(&created(500, 20), 100, 0))
        .expect("create");

    let topup = BatchTopUp {
        batchId: BATCH,
        topupAmount: U256::from(250u64),
        normalisedBalance: U256::from(800u64),
    };
    indexer.apply(150, &log_for(&topup, 150, 2)).expect("topup");

    let batch = read_batch(db.as_ref(), BATCH).unwrap().unwrap();
    assert_eq!(batch.normalised_balance, U256::from(800u64));
    assert_eq!(
        batch.start_block, 100,
        "start block is immutable after create"
    );
    assert_eq!(batch.depth, 20, "topup leaves depth untouched");
}

#[test]
fn batch_depth_increase_folds_depth_and_balance() {
    let (indexer, db) = new_indexer();
    indexer
        .apply(100, &log_for(&created(500, 20), 100, 0))
        .expect("create");

    let bump = BatchDepthIncrease {
        batchId: BATCH,
        newDepth: 22,
        normalisedBalance: U256::from(450u64),
    };
    indexer.apply(160, &log_for(&bump, 160, 1)).expect("bump");

    let batch = read_batch(db.as_ref(), BATCH).unwrap().unwrap();
    assert_eq!(batch.depth, 22);
    assert_eq!(batch.normalised_balance, U256::from(450u64));
}

#[test]
fn topup_before_create_is_dropped() {
    let (indexer, db) = new_indexer();
    let topup = BatchTopUp {
        batchId: BATCH,
        topupAmount: U256::from(250u64),
        normalisedBalance: U256::from(800u64),
    };
    indexer.apply(150, &log_for(&topup, 150, 0)).expect("topup");
    assert!(
        read_batch(db.as_ref(), BATCH).unwrap().is_none(),
        "a topup with no prior create fabricates no partial row"
    );
}

#[test]
fn price_update_folds_chain_state_accumulator() {
    let (indexer, db) = new_indexer();

    // First price update: lastPrice was zero, so totalOutPayment does not move.
    indexer
        .apply(
            200,
            &log_for(
                &PriceUpdate {
                    price: U256::from(10u64),
                },
                200,
                0,
            ),
        )
        .expect("first price");
    let state = read_chain_state(db.as_ref()).unwrap().unwrap();
    assert_eq!(state.last_price, U256::from(10u64));
    assert_eq!(state.last_updated_block, 200);
    assert_eq!(state.total_out_payment, U256::ZERO);

    // Second price update at block 210: settle 10 * (210 - 200) = 100 into the
    // accumulator, then adopt the new price.
    indexer
        .apply(
            210,
            &log_for(
                &PriceUpdate {
                    price: U256::from(20u64),
                },
                210,
                0,
            ),
        )
        .expect("second price");
    let state = read_chain_state(db.as_ref()).unwrap().unwrap();
    assert_eq!(state.total_out_payment, U256::from(100u64));
    assert_eq!(state.last_price, U256::from(20u64));
    assert_eq!(state.last_updated_block, 210);

    // currentTotalOutPayment(260) = 100 + 20 * (260 - 210) = 1100.
    assert_eq!(
        state.current_total_out_payment(260),
        U256::from(1_100u64),
        "matches the contract's currentTotalOutPayment arithmetic"
    );
}

#[test]
fn paused_sets_the_flag() {
    let (indexer, db) = new_indexer();
    indexer
        .apply(300, &log_for(&Paused { account: OWNER }, 300, 0))
        .expect("paused");
    let state = read_chain_state(db.as_ref()).unwrap().unwrap();
    assert!(state.paused);
}

#[test]
fn is_batch_valid_now_tracks_the_rising_line() {
    let (indexer, db) = new_indexer();

    // Batch with normalisedBalance 1000.
    indexer
        .apply(100, &log_for(&created(1_000, 20), 100, 0))
        .expect("create");
    // Price 10/block anchored at block 200, totalOutPayment 0.
    indexer
        .apply(
            200,
            &log_for(
                &PriceUpdate {
                    price: U256::from(10u64),
                },
                200,
                0,
            ),
        )
        .expect("price");

    // At block 250: currentTotalOutPayment = 0 + 10 * 50 = 500 < 1000 -> valid.
    assert_eq!(
        is_batch_valid_now(db.as_ref(), BATCH, 250).unwrap(),
        Some(true)
    );
    // At block 300: 10 * 100 = 1000, not strictly greater -> invalid.
    assert_eq!(
        is_batch_valid_now(db.as_ref(), BATCH, 300).unwrap(),
        Some(false)
    );
    // At block 400: 10 * 200 = 2000 > 1000 -> invalid.
    assert_eq!(
        is_batch_valid_now(db.as_ref(), BATCH, 400).unwrap(),
        Some(false)
    );

    // An unknown batch is not-yet-known, not expired.
    assert_eq!(
        is_batch_valid_now(db.as_ref(), B256::repeat_byte(0xff), 250).unwrap(),
        None
    );
}

#[test]
fn is_valid_now_method_matches_helper() {
    let state = ChainState {
        total_out_payment: U256::from(100u64),
        last_price: U256::from(20u64),
        last_updated_block: 210,
        paused: false,
        source: Default::default(),
    };
    // currentTotalOutPayment(260) = 100 + 20*50 = 1100.
    assert_eq!(state.current_total_out_payment(260), U256::from(1_100u64));
    // A query before the anchor block contributes no elapsed cost (saturating).
    assert_eq!(state.current_total_out_payment(200), U256::from(100u64));
}

#[test]
fn reapplying_a_create_is_idempotent() {
    let (indexer, db) = new_indexer();
    let log = log_for(&created(500, 20), 100, 0);
    indexer.apply(100, &log).expect("first");
    let first = read_batch(db.as_ref(), BATCH).unwrap();
    indexer.apply(100, &log).expect("second");
    let second = read_batch(db.as_ref(), BATCH).unwrap();
    assert_eq!(first, second, "re-applying a log re-writes the same row");
}

#[test]
fn reapplying_a_price_update_is_idempotent() {
    let (indexer, db) = new_indexer();
    indexer
        .apply(
            200,
            &log_for(
                &PriceUpdate {
                    price: U256::from(10u64),
                },
                200,
                0,
            ),
        )
        .expect("first");
    let log2 = log_for(
        &PriceUpdate {
            price: U256::from(20u64),
        },
        210,
        0,
    );
    indexer.apply(210, &log2).expect("second");
    let after_first = read_chain_state(db.as_ref()).unwrap();
    // Replay the second update: the accumulator must not double-advance.
    indexer.apply(210, &log2).expect("replay");
    let after_replay = read_chain_state(db.as_ref()).unwrap();
    assert_eq!(
        after_first, after_replay,
        "a replayed price update does not re-fold the accumulator"
    );
}

#[test]
fn supersede_is_monotonic_by_position() {
    let (indexer, db) = new_indexer();
    indexer
        .apply(100, &log_for(&created(500, 20), 100, 0))
        .expect("create");

    // A topup at an EARLIER position than the create must be ignored.
    let stale = BatchTopUp {
        batchId: BATCH,
        topupAmount: U256::from(1u64),
        normalisedBalance: U256::from(999u64),
    };
    // Same block, earlier log_index than the create (create was at index 0; a
    // strictly-earlier index is impossible, so use an earlier block instead).
    indexer.apply(50, &log_for(&stale, 50, 5)).expect("stale");
    let batch = read_batch(db.as_ref(), BATCH).unwrap().unwrap();
    assert_eq!(
        batch.normalised_balance,
        U256::from(500u64),
        "an earlier-positioned log never rolls the row back"
    );

    // A topup at a LATER position supersedes.
    let fresh = BatchTopUp {
        batchId: BATCH,
        topupAmount: U256::from(1u64),
        normalisedBalance: U256::from(700u64),
    };
    indexer.apply(120, &log_for(&fresh, 120, 0)).expect("fresh");
    let batch = read_batch(db.as_ref(), BATCH).unwrap().unwrap();
    assert_eq!(batch.normalised_balance, U256::from(700u64));
}

#[test]
fn filter_selects_the_contract_and_all_events() {
    let (indexer, _db) = new_indexer();
    let filter = indexer.filter();
    let addrs: Vec<_> = filter.address.iter().collect();
    assert!(addrs.contains(&&POSTAGE_STAMP_ADDRESS));

    let topic0 = filter.topics[0].iter().collect::<Vec<_>>();
    for sig in [
        BatchCreated::SIGNATURE_HASH,
        BatchTopUp::SIGNATURE_HASH,
        BatchDepthIncrease::SIGNATURE_HASH,
        PriceUpdate::SIGNATURE_HASH,
        Paused::SIGNATURE_HASH,
    ] {
        assert!(topic0.contains(&&sig), "topic0 set includes every event");
    }
}
