//! Postage index tests: the batch fold into [`Batches`] (read back via
//! [`DbBatchStore`]), the `total_out_payment_at` price fold, and revert.

use std::sync::Arc;

use alloy_primitives::{Address, B256, LogData, U256};
use alloy_rpc_types_eth::Log;
use alloy_sol_types::SolEvent;
use nectar_postage::{BatchStore, PostageContext};
use vertex_chain_index::Indexer;
use vertex_chain_index_framework::{ContractIndexer, Network};
use vertex_storage_redb::RedbDatabase;

use crate::DbBatchStore;
use crate::index::abi::events;
use crate::index::{registration, total_out_payment_at};

// Synthetic address, distinct from mainnet so a test never matches a real deployment.
const POSTAGE_ADDR: Address = Address::repeat_byte(0xa0);

fn harness() -> (Arc<RedbDatabase>, ContractIndexer<RedbDatabase>) {
    let db = Arc::new(RedbDatabase::in_memory().expect("in-memory db"));
    let reg = registration::<RedbDatabase>(Network::Mainnet);
    let reg = override_address(reg, POSTAGE_ADDR);
    let indexer = ContractIndexer::from_registrations(db.clone(), vec![reg]).expect("indexer");
    (db, indexer)
}

// Re-home the watched contract onto the synthetic address, keeping tag, events,
// and reducer.
fn override_address(
    mut reg: vertex_chain_index_framework::DomainRegistration<RedbDatabase>,
    address: Address,
) -> vertex_chain_index_framework::DomainRegistration<RedbDatabase> {
    for c in &mut reg.contracts {
        c.address = address;
        c.start_block = 0;
    }
    reg
}

fn log_from(block: u64, index: u64, data: LogData) -> Log {
    Log {
        inner: alloy_primitives::Log {
            address: POSTAGE_ADDR,
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

fn apply<E: SolEvent>(indexer: &ContractIndexer<RedbDatabase>, block: u64, index: u64, event: &E) {
    let log = log_from(block, index, event.encode_log_data());
    indexer.apply(block, &log).expect("apply");
}

fn store(db: &Arc<RedbDatabase>) -> DbBatchStore<RedbDatabase> {
    DbBatchStore::new(db.clone()).expect("batch store")
}

#[test]
fn registration_builds_indexer() {
    let db = Arc::new(RedbDatabase::in_memory().expect("db"));
    let indexer = ContractIndexer::from_registrations(db, vec![registration(Network::Mainnet)])
        .expect("postage registration must compose into the unified indexer");
    assert_eq!(
        indexer.filter().address.len(),
        1,
        "one watched PostageStamp"
    );
}

#[test]
fn batch_created_topup_and_dilute_track_the_store() {
    let (db, indexer) = harness();
    let id = B256::repeat_byte(0x11);
    let owner = Address::repeat_byte(0x22);

    apply(
        &indexer,
        10,
        0,
        &events::BatchCreated {
            batchId: id,
            totalAmount: U256::from(1_000),
            normalisedBalance: U256::from(500),
            owner,
            depth: 20,
            bucketDepth: 16,
            immutableFlag: false,
        },
    );

    let batch = store(&db).get(&id).unwrap().expect("batch created");
    assert_eq!(batch.value(), 500);
    assert_eq!(batch.depth(), 20);
    assert_eq!(batch.owner(), owner);
    assert_eq!(batch.start(), 10, "start is the creation block");

    apply(
        &indexer,
        20,
        0,
        &events::BatchTopUp {
            batchId: id,
            topupAmount: U256::from(250),
            normalisedBalance: U256::from(750),
        },
    );
    assert_eq!(store(&db).get(&id).unwrap().unwrap().value(), 750);

    apply(
        &indexer,
        30,
        0,
        &events::BatchDepthIncrease {
            batchId: id,
            newDepth: 24,
            normalisedBalance: U256::from(375),
        },
    );
    let batch = store(&db).get(&id).unwrap().unwrap();
    assert_eq!(batch.depth(), 24, "dilution raised the depth");
    assert_eq!(batch.value(), 375, "dilution re-normalised the balance");
}

#[test]
fn mutation_for_unknown_batch_is_dropped() {
    let (db, indexer) = harness();
    let id = B256::repeat_byte(0x33);
    apply(
        &indexer,
        5,
        0,
        &events::BatchTopUp {
            batchId: id,
            topupAmount: U256::from(1),
            normalisedBalance: U256::from(9),
        },
    );
    assert!(
        store(&db).get(&id).unwrap().is_none(),
        "a top-up without a prior create fabricates no row"
    );
}

#[test]
fn total_out_payment_folds_the_price_cadence() {
    let (db, indexer) = harness();
    apply(
        &indexer,
        100,
        0,
        &events::PriceUpdate {
            price: U256::from(10),
        },
    );
    apply(
        &indexer,
        200,
        0,
        &events::PriceUpdate {
            price: U256::from(20),
        },
    );

    // Before the first update nothing has accrued.
    assert_eq!(total_out_payment_at(&*db, 50).unwrap(), 0);
    // Within the first interval: 10 * (150 - 100).
    assert_eq!(total_out_payment_at(&*db, 150).unwrap(), 500);
    // Past the second update: 10 * 100 + 20 * (300 - 200).
    assert_eq!(total_out_payment_at(&*db, 300).unwrap(), 3_000);
    // A batch whose context is unset reads the default (zero), so expiry needs a
    // block-clock consumer to publish this figure into the PostageContext.
    assert_eq!(store(&db).context().unwrap(), PostageContext::default());
}

#[test]
fn revert_rebuilds_the_batch_projection_from_survivors() {
    let (db, indexer) = harness();
    let early = B256::repeat_byte(0x01);
    let late = B256::repeat_byte(0x02);

    let create = |id| events::BatchCreated {
        batchId: id,
        totalAmount: U256::from(1_000),
        normalisedBalance: U256::from(500),
        owner: Address::repeat_byte(0x44),
        depth: 20,
        bucketDepth: 16,
        immutableFlag: false,
    };
    apply(&indexer, 50, 0, &create(early));
    apply(&indexer, 150, 0, &create(late));
    assert!(store(&db).get(&late).unwrap().is_some());

    indexer.revert(100).expect("revert");
    assert!(
        store(&db).get(&late).unwrap().is_none(),
        "the in-range create is reverted from the projection"
    );
    assert!(
        store(&db).get(&early).unwrap().is_some(),
        "the out-of-range create survives"
    );
}
