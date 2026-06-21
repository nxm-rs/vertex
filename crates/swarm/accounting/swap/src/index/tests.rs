//! Swap-domain tests: the lazy two-scalar settlement views and revert over a
//! synthetic registration.

use std::sync::Arc;

use alloy_primitives::{Address, B256, LogData, U256, address};
use alloy_rpc_types_eth::Log;
use alloy_sol_types::SolEvent;
use nectar_contracts::ISwapPriceOracle;
use vertex_chain_index::Indexer;
use vertex_chain_index_framework::{
    ContractIndexer, DomainRegistration, EventDescriptor, Network, WatchedContract,
};
use vertex_storage_redb::RedbDatabase;

use crate::index::register::TAG_SWAP_ORACLE;
use crate::index::views;

// Synthetic address, distinct from any real deployment.
const SWAP_ADDR: Address = address!("0000000000000000000000000000000000000a04");

const SWAP_EVENTS: &[EventDescriptor] = &[
    EventDescriptor {
        topic0: ISwapPriceOracle::PriceUpdate::SIGNATURE_HASH,
        name: "PriceUpdate",
    },
    EventDescriptor {
        topic0: ISwapPriceOracle::ChequeValueDeductionUpdate::SIGNATURE_HASH,
        name: "ChequeValueDeductionUpdate",
    },
];

fn test_registration<DB: vertex_storage::Database>() -> DomainRegistration<DB> {
    DomainRegistration {
        contracts: vec![WatchedContract {
            tag: TAG_SWAP_ORACLE,
            address: SWAP_ADDR,
            start_block: 0,
            events: SWAP_EVENTS,
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
    let db = Arc::new(RedbDatabase::in_memory().expect("db"));
    let indexer =
        ContractIndexer::from_registrations(db, vec![crate::index::registration(Network::Mainnet)])
            .expect("swap registration must compose into the unified indexer");
    let filter = indexer.filter();
    assert_eq!(filter.address.len(), 1, "single swap-oracle address");
}

#[test]
fn swap_two_scalars_last_write_wins() {
    let (db, indexer) = harness();
    assert_eq!(views::exchange_rate(&*db).unwrap(), None);
    assert_eq!(views::cheque_value_deduction(&*db).unwrap(), None);

    apply(
        &indexer,
        1,
        0,
        SWAP_ADDR,
        &ISwapPriceOracle::PriceUpdate {
            price: U256::from(10u64),
        },
    );
    apply(
        &indexer,
        2,
        0,
        SWAP_ADDR,
        &ISwapPriceOracle::PriceUpdate {
            price: U256::from(20u64),
        },
    );
    apply(
        &indexer,
        3,
        0,
        SWAP_ADDR,
        &ISwapPriceOracle::ChequeValueDeductionUpdate {
            chequeValueDeduction: U256::from(5u64),
        },
    );
    assert_eq!(views::exchange_rate(&*db).unwrap(), Some(U256::from(20u64)));
    assert_eq!(
        views::cheque_value_deduction(&*db).unwrap(),
        Some(U256::from(5u64))
    );
}

#[test]
fn revert_range_deletes_per_contract() {
    let (db, indexer) = harness();
    apply(
        &indexer,
        100,
        0,
        SWAP_ADDR,
        &ISwapPriceOracle::PriceUpdate {
            price: U256::from(1u64),
        },
    );
    apply(
        &indexer,
        200,
        0,
        SWAP_ADDR,
        &ISwapPriceOracle::PriceUpdate {
            price: U256::from(2u64),
        },
    );
    assert_eq!(views::exchange_rate(&*db).unwrap(), Some(U256::from(2u64)));

    // Revert from 150 drops the block-200 update, keeps block-100.
    indexer.revert(150).expect("revert");
    assert_eq!(views::exchange_rate(&*db).unwrap(), Some(U256::from(1u64)));
}

#[test]
fn descriptor_topic0s_are_the_signature_hashes() {
    // Each descriptor's topic0 is the event's SIGNATURE_HASH (the match authority).
    let [price, deduction] = SWAP_EVENTS else {
        panic!("expected exactly the two swap-oracle descriptors");
    };
    assert_eq!(price.topic0, ISwapPriceOracle::PriceUpdate::SIGNATURE_HASH);
    assert_eq!(
        deduction.topic0,
        ISwapPriceOracle::ChequeValueDeductionUpdate::SIGNATURE_HASH
    );
}
