//! Chequebook-domain tests: the lazy factory-deployed-set fold and revert over a
//! synthetic registration.

use std::sync::Arc;

use alloy_primitives::{Address, B256, LogData, address};
use alloy_rpc_types_eth::Log;
use alloy_sol_types::SolEvent;
use nectar_contracts::IChequebookFactory;
use vertex_chain_index::Indexer;
use vertex_chain_index_framework::{
    ContractIndexer, DomainRegistration, EventDescriptor, Network, WatchedContract,
};
use vertex_storage_redb::RedbDatabase;

use crate::index::{TAG_CHEQUEBOOK, views};

// A synthetic chequebook-factory address, distinct from mainnet so a test never
// accidentally matches a real deployment.
const CHEQUEBOOK_ADDR: Address = address!("0000000000000000000000000000000000000a04");

// The event descriptor set, mirroring the production registration, so a synthetic
// registration can reuse it at a synthetic address.
const CHEQUEBOOK_EVENTS: &[EventDescriptor] = &[EventDescriptor {
    topic0: IChequebookFactory::SimpleSwapDeployed::SIGNATURE_HASH,
    name: "SimpleSwapDeployed",
}];

fn test_registration<DB: vertex_storage::Database>() -> DomainRegistration<DB> {
    DomainRegistration {
        contracts: vec![WatchedContract {
            tag: TAG_CHEQUEBOOK,
            address: CHEQUEBOOK_ADDR,
            start_block: 0,
            events: CHEQUEBOOK_EVENTS,
        }],
        reducers: vec![],
        tables: &[],
    }
}

fn harness() -> (Arc<RedbDatabase>, ContractIndexer<RedbDatabase>) {
    let db = Arc::new(RedbDatabase::in_memory().expect("in-memory db"));
    let indexer = ContractIndexer::from_registrations(db.clone(), vec![test_registration()])
        .expect("indexer");
    (db, indexer)
}

fn log_from(block: u64, index: u64, address: Address, data: LogData) -> Log {
    Log {
        inner: alloy_primitives::Log { address, data },
        block_hash: Some(B256::repeat_byte(block as u8)),
        block_number: Some(block),
        block_timestamp: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: Some(index),
        removed: false,
    }
}

fn apply<E: SolEvent>(
    indexer: &ContractIndexer<RedbDatabase>,
    block: u64,
    index: u64,
    address: Address,
    event: &E,
) {
    let log = log_from(block, index, address, event.encode_log_data());
    indexer.apply(block, &log).expect("apply");
}

#[test]
fn registration_builds_indexer() {
    // The production registration with the real canonical address composes
    // cleanly into the unified indexer.
    let db = Arc::new(RedbDatabase::in_memory().expect("db"));
    let indexer =
        ContractIndexer::from_registrations(db, vec![crate::index::registration(Network::Mainnet)])
            .expect("chequebook registration must compose into the unified indexer");
    let filter = indexer.filter();
    assert_eq!(
        filter.address.len(),
        1,
        "the single chequebook-factory address"
    );
}

#[test]
fn chequebook_membership() {
    let (db, indexer) = harness();
    let cb = address!("00000000000000000000000000000000000000c1");
    assert!(!views::is_factory_deployed(&*db, cb).unwrap());
    apply(
        &indexer,
        7,
        0,
        CHEQUEBOOK_ADDR,
        &IChequebookFactory::SimpleSwapDeployed {
            contractAddress: cb,
        },
    );
    assert!(views::is_factory_deployed(&*db, cb).unwrap());
    assert_eq!(views::deployed_chequebooks(&*db).unwrap(), vec![cb]);
}

#[test]
fn deployed_set_collects_every_chequebook() {
    let (db, indexer) = harness();
    let a = address!("00000000000000000000000000000000000000a1");
    let b = address!("00000000000000000000000000000000000000b2");
    apply(
        &indexer,
        1,
        0,
        CHEQUEBOOK_ADDR,
        &IChequebookFactory::SimpleSwapDeployed { contractAddress: a },
    );
    apply(
        &indexer,
        1,
        1,
        CHEQUEBOOK_ADDR,
        &IChequebookFactory::SimpleSwapDeployed { contractAddress: b },
    );
    let mut got = views::deployed_chequebooks(&*db).unwrap();
    got.sort();
    let mut expected = vec![a, b];
    expected.sort();
    assert_eq!(got, expected);
}

#[test]
fn revert_drops_out_of_range_deployments() {
    let (db, indexer) = harness();
    let early = address!("00000000000000000000000000000000000000e1");
    let late = address!("00000000000000000000000000000000000000f2");
    apply(
        &indexer,
        50,
        0,
        CHEQUEBOOK_ADDR,
        &IChequebookFactory::SimpleSwapDeployed {
            contractAddress: early,
        },
    );
    apply(
        &indexer,
        150,
        0,
        CHEQUEBOOK_ADDR,
        &IChequebookFactory::SimpleSwapDeployed {
            contractAddress: late,
        },
    );
    assert!(views::is_factory_deployed(&*db, late).unwrap());

    indexer.revert(100).expect("revert");
    assert!(
        !views::is_factory_deployed(&*db, late).unwrap(),
        "the in-range deployment must be reverted from the verbatim store"
    );
    assert!(
        views::is_factory_deployed(&*db, early).unwrap(),
        "the out-of-range deployment must survive the revert"
    );
}
