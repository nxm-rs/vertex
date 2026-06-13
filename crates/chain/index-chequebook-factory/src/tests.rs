//! Unit tests over synthetic logs (no chain).
//!
//! These build canonical `SimpleSwapDeployed` logs by ABI-encoding the event,
//! fold them through [`ChequebookFactoryIndexer::apply`], and assert the
//! projection records membership. They also assert the fold is idempotent (replay
//! is a no-op) and monotonic (a stale, reordered log never regresses the row),
//! that distinct deployments accumulate, and that an unrelated topic is ignored.

use std::sync::Arc;

use alloy_primitives::{Address, B256, LogData};
use alloy_rpc_types_eth::Log;
use alloy_sol_types::SolEvent;
use nectar_contracts::{ChequebookFactory, IChequebookFactory};
use vertex_chain_index::Indexer;
use vertex_storage_redb::RedbDatabase;

use crate::indexer::ChequebookFactoryIndexer;
use crate::projection::{LogPosition, deployment_of, is_factory_deployed};

/// The factory contract address used in the synthetic logs.
const FACTORY: Address = Address::repeat_byte(0xfa);

/// Two distinct deployed-chequebook addresses.
const CHEQUEBOOK_A: Address = Address::repeat_byte(0xa1);
const CHEQUEBOOK_B: Address = Address::repeat_byte(0xb2);

/// A deployment fixture: the test factory address, deployment block 0.
fn deployment() -> ChequebookFactory {
    ChequebookFactory::new(FACTORY, 0)
}

/// Build an indexer over a fresh in-memory database with its table created.
fn fresh_indexer() -> (ChequebookFactoryIndexer<RedbDatabase>, Arc<RedbDatabase>) {
    let db = Arc::new(RedbDatabase::in_memory().expect("in-memory db"));
    let indexer = ChequebookFactoryIndexer::new(deployment(), db.clone());
    indexer.init().expect("init projection table");
    (indexer, db)
}

/// A synthetic `SimpleSwapDeployed` log at `(block, index)` for `contract`.
fn deployed_log(block: u64, index: u64, contract: Address) -> Log {
    let event = IChequebookFactory::SimpleSwapDeployed {
        contractAddress: contract,
    };
    let topic0 = event.encode_topics_array::<1>()[0].into();
    log_for(block, index, topic0, event)
}

/// Wrap an encoded event into an RPC [`Log`] at a chosen position.
fn log_for<E: SolEvent>(block: u64, index: u64, topic0: B256, event: E) -> Log {
    let topics = vec![topic0];
    let data = event.encode_data().into();
    Log {
        inner: alloy_primitives::Log {
            address: FACTORY,
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
fn filter_selects_the_deployment_event() {
    let (indexer, _db) = fresh_indexer();
    let filter = indexer.filter();
    assert!(
        filter.topics[0].matches(&IChequebookFactory::SimpleSwapDeployed::SIGNATURE_HASH),
        "filter selects SimpleSwapDeployed"
    );
    let addrs: Vec<_> = filter.address.iter().collect();
    assert!(
        addrs.contains(&&FACTORY),
        "filter constrains to the factory"
    );
}

#[test]
fn deployment_folds_into_the_set() {
    let (indexer, db) = fresh_indexer();

    indexer
        .apply(40_000_000, &deployed_log(40_000_000, 2, CHEQUEBOOK_A))
        .expect("apply deployment");

    assert!(
        is_factory_deployed(db.as_ref(), CHEQUEBOOK_A).expect("read membership"),
        "the deployed chequebook is in the set"
    );
    let row = deployment_of(db.as_ref(), CHEQUEBOOK_A)
        .expect("read row")
        .expect("row present");
    assert_eq!(
        row.source,
        LogPosition {
            block: 40_000_000,
            log_index: 2
        }
    );

    // An address the factory never deployed is not in the set.
    assert!(
        !is_factory_deployed(db.as_ref(), CHEQUEBOOK_B).expect("read membership"),
        "an unseen chequebook is not in the set"
    );
}

#[test]
fn distinct_deployments_accumulate() {
    let (indexer, db) = fresh_indexer();

    indexer
        .apply(100, &deployed_log(100, 0, CHEQUEBOOK_A))
        .unwrap();
    indexer
        .apply(101, &deployed_log(101, 0, CHEQUEBOOK_B))
        .unwrap();

    assert!(is_factory_deployed(db.as_ref(), CHEQUEBOOK_A).unwrap());
    assert!(is_factory_deployed(db.as_ref(), CHEQUEBOOK_B).unwrap());
}

#[test]
fn reapplying_the_same_log_is_idempotent() {
    let (indexer, db) = fresh_indexer();
    let log = deployed_log(200, 3, CHEQUEBOOK_A);

    // Apply the identical finalized log twice: the second is a no-op replay.
    indexer.apply(200, &log).unwrap();
    let after_first = deployment_of(db.as_ref(), CHEQUEBOOK_A).unwrap().unwrap();
    indexer.apply(200, &log).unwrap();
    let after_second = deployment_of(db.as_ref(), CHEQUEBOOK_A).unwrap().unwrap();

    assert_eq!(after_first, after_second, "replay leaves the row unchanged");
}

#[test]
fn stale_log_does_not_regress_the_source() {
    let (indexer, db) = fresh_indexer();

    // A later log for the same address records the newer source position.
    indexer
        .apply(305, &deployed_log(305, 1, CHEQUEBOOK_A))
        .unwrap();
    // Re-delivering an earlier log (a reorder/replay) must not roll the source
    // back, even though the address stays in the set either way.
    indexer
        .apply(300, &deployed_log(300, 0, CHEQUEBOOK_A))
        .unwrap();

    let row = deployment_of(db.as_ref(), CHEQUEBOOK_A).unwrap().unwrap();
    assert_eq!(
        row.source,
        LogPosition {
            block: 305,
            log_index: 1
        },
        "stale log does not overwrite the newer source"
    );
    assert!(is_factory_deployed(db.as_ref(), CHEQUEBOOK_A).unwrap());
}

#[test]
fn same_block_higher_log_index_supersedes() {
    let (indexer, db) = fresh_indexer();

    indexer
        .apply(400, &deployed_log(400, 0, CHEQUEBOOK_A))
        .unwrap();
    indexer
        .apply(400, &deployed_log(400, 5, CHEQUEBOOK_A))
        .unwrap();

    let row = deployment_of(db.as_ref(), CHEQUEBOOK_A).unwrap().unwrap();
    assert_eq!(row.source.log_index, 5);
}

#[test]
fn unrelated_topic_is_ignored() {
    let (indexer, db) = fresh_indexer();

    // A log whose topic0 matches no indexed event must be a no-op.
    let other = Log {
        inner: alloy_primitives::Log {
            address: FACTORY,
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
        !is_factory_deployed(db.as_ref(), CHEQUEBOOK_A).unwrap(),
        "no deployment was recorded"
    );
}
