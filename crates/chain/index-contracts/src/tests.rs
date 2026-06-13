//! Table-driven synthetic-log tests over the unified indexer.
//!
//! One harness builds a synthetic [`Log`] for a `(contract, event)` pair, feeds
//! it through [`ContractIndexer::apply`], and asserts a view answer. This is the
//! single test shell the five per-branch `tests.rs` collapse into; the fixtures
//! are per-contract, the harness is shared.

use std::sync::Arc;

use alloy_primitives::{Address, B256, Bytes, LogData, U256, address};
use alloy_rpc_types_eth::Log;
use alloy_sol_types::SolEvent;
use vertex_chain_index::Indexer;
use vertex_storage::{Database, DbTx};
use vertex_storage_redb::RedbDatabase;

use crate::registry::{ContractId, EventDescriptor, WatchedContract, abi};
use crate::store::{EventKey, EventTable, MAX_EVENT_DATA};
use crate::views::{chequebook, postage, redistribution, staking, swap};
use crate::{ContractIndexer, views};

// Synthetic addresses for the watched contracts, distinct from mainnet so a
// test never accidentally matches a real deployment.
const POSTAGE_ADDR: Address = address!("0000000000000000000000000000000000000a01");
const STAKING_ADDR: Address = address!("0000000000000000000000000000000000000a02");
const REDIST_ADDR: Address = address!("0000000000000000000000000000000000000a03");
const CHEQUEBOOK_ADDR: Address = address!("0000000000000000000000000000000000000a04");
const SWAP_ADDR: Address = address!("0000000000000000000000000000000000000a05");

/// A test registry over the synthetic addresses, with every contract starting
/// at block 0 so the harness can place events anywhere.
fn test_contracts() -> Vec<WatchedContract> {
    use crate::registry::registry;
    // Start from the real registry to inherit the event descriptor sets, then
    // repoint each contract at a synthetic address and a block-0 start.
    let mut contracts = registry(crate::registry::Network::Mainnet);
    for c in &mut contracts {
        c.start_block = 0;
        c.address = match c.id {
            ContractId::Postage => POSTAGE_ADDR,
            ContractId::Staking => STAKING_ADDR,
            ContractId::Redistribution => REDIST_ADDR,
            ContractId::ChequebookFactory => CHEQUEBOOK_ADDR,
            ContractId::SwapPriceOracle => SWAP_ADDR,
            ContractId::StoragePriceOracle => {
                address!("0000000000000000000000000000000000000a06")
            }
        };
    }
    contracts
}

/// Build a synthetic [`Log`] at `(block, index)` from `address` carrying
/// `data`'s ABI bytes.
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

/// Build a synthetic indexer over an in-memory database and the test registry.
fn harness() -> (Arc<RedbDatabase>, ContractIndexer<RedbDatabase>) {
    let db = Arc::new(RedbDatabase::in_memory().expect("in-memory db"));
    let indexer = ContractIndexer::with_contracts(db.clone(), test_contracts()).expect("indexer");
    (db, indexer)
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
fn filter_unions_addresses_and_topics() {
    let (_db, indexer) = harness();
    let filter = indexer.filter();
    // Six watched contracts, six addresses.
    assert_eq!(filter.address.len(), 6);
    // The topic0 set is the union of every contract's events; non-empty.
    let topics = filter.topics[0].to_value_or_array();
    assert!(topics.is_some());
}

#[test]
fn unwatched_address_is_skipped() {
    let (db, indexer) = harness();
    let stray = address!("00000000000000000000000000000000deadbeef");
    let ev = abi::PriceUpdate {
        price: U256::from(7u64),
    };
    let log = log_from(10, 0, stray, ev.encode_log_data());
    indexer.apply(10, &log).expect("apply");
    let count = db.view(|tx| tx.count::<EventTable>()).unwrap();
    assert_eq!(
        count, 0,
        "a log from an unwatched address must not be stored"
    );
}

#[test]
fn topic_not_declared_for_resolved_contract_is_skipped() {
    // Emit a chequebook `SimpleSwapDeployed` at the postage address. The address
    // resolves to Postage, which does NOT declare that topic0, so the row must
    // be skipped (never misfiled under Postage).
    let (db, indexer) = harness();
    let cb = address!("00000000000000000000000000000000000000c9");
    let ev = nectar_contracts::IChequebookFactory::SimpleSwapDeployed {
        contractAddress: cb,
    };
    let postage = test_contracts()
        .into_iter()
        .find(|c| c.id == ContractId::Postage)
        .unwrap();
    assert!(
        !postage.declares(nectar_contracts::IChequebookFactory::SimpleSwapDeployed::SIGNATURE_HASH),
        "fixture invalid: postage must not declare the chequebook topic0"
    );
    let log = log_from(5, 0, POSTAGE_ADDR, ev.encode_log_data());
    indexer.apply(5, &log).expect("apply");
    let stored = db
        .view(|tx| {
            tx.get::<EventTable>(EventKey {
                contract: ContractId::Postage,
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
    // Hand-build a postage-addressed log whose topic0 is a declared event but
    // whose data exceeds the cap.
    let topic0 = abi::PriceUpdate::SIGNATURE_HASH;
    let huge = Bytes::from(vec![0u8; MAX_EVENT_DATA + 1]);
    let data = LogData::new_unchecked(vec![topic0], huge);
    let log = log_from(9, 0, POSTAGE_ADDR, data);
    indexer
        .apply(9, &log)
        .expect("apply must not error on oversized data");
    let stored = db
        .view(|tx| {
            tx.get::<EventTable>(EventKey {
                contract: ContractId::Postage,
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
    let ev = abi::PriceUpdate {
        price: U256::from(1u64),
    };
    let mut log = log_from(3, 0, POSTAGE_ADDR, ev.encode_log_data());
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
    let ev = abi::PriceUpdate {
        price: U256::from(42u64),
    };
    apply(&indexer, 100, 0, POSTAGE_ADDR, &ev);
    apply(&indexer, 100, 0, POSTAGE_ADDR, &ev); // replay same position
    let count = db.view(|tx| tx.count::<EventTable>()).unwrap();
    assert_eq!(
        count, 1,
        "a replayed (block, log_index) overwrites in place"
    );
}

#[test]
fn postage_validity_fold() {
    let (db, indexer) = harness();
    let batch_id = B256::repeat_byte(0xAB);

    // Price update at block 100: price 2 per chunk per block.
    apply(
        &indexer,
        100,
        0,
        POSTAGE_ADDR,
        &abi::PriceUpdate {
            price: U256::from(2u64),
        },
    );
    // Create a batch at block 100 with normalisedBalance 1000.
    apply(
        &indexer,
        100,
        1,
        POSTAGE_ADDR,
        &abi::BatchCreated {
            batchId: batch_id,
            totalAmount: U256::from(1000u64),
            normalisedBalance: U256::from(1000u64),
            owner: address!("00000000000000000000000000000000000000aa"),
            depth: 20,
            bucketDepth: 16,
            immutableFlag: false,
        },
    );

    // At block 100, currentTotalOutPayment = 0 (last_price just set), so valid.
    assert_eq!(
        postage::is_batch_valid_now(&*db, batch_id, 100).unwrap(),
        Some(true)
    );
    // At block 600: total_out_payment grows by 2*(600-100)=1000, not strictly
    // below 1000, so invalid.
    assert_eq!(
        postage::is_batch_valid_now(&*db, batch_id, 600).unwrap(),
        Some(false)
    );
    // An unknown batch is not-yet-known.
    assert_eq!(
        postage::is_batch_valid_now(&*db, B256::ZERO, 100).unwrap(),
        None
    );

    // The value-sorted hint surfaces the created batch.
    let candidates = postage::eviction_candidates(&*db, 10).unwrap();
    assert_eq!(candidates, vec![batch_id]);

    // A topup raises the balance; the fold reflects it and the index moves.
    apply(
        &indexer,
        200,
        0,
        POSTAGE_ADDR,
        &abi::BatchTopUp {
            batchId: batch_id,
            topupAmount: U256::from(5000u64),
            normalisedBalance: U256::from(6000u64),
        },
    );
    assert_eq!(
        postage::batch(&*db, batch_id)
            .unwrap()
            .unwrap()
            .normalised_balance,
        U256::from(6000u64)
    );
    assert_eq!(
        postage::batch_state(&*db, batch_id)
            .unwrap()
            .unwrap()
            .normalised_balance,
        U256::from(6000u64)
    );
}

#[test]
fn two_batches_same_balance_both_survive_eviction_hint() {
    // Regression: the value-sorted eviction index keyed on `normalisedBalance`
    // alone collided two batches with equal balance, silently dropping one from
    // the hint so an expired batch sharing a balance could never be evicted. The
    // key now includes `batchId`, so both survive in balance order.
    let (db, indexer) = harness();
    let batch_a = B256::repeat_byte(0xAA);
    let batch_b = B256::repeat_byte(0xBB);

    // A price must be indexed for is_batch_valid_now to answer; the eviction hint
    // itself only needs the two creates.
    apply(
        &indexer,
        100,
        0,
        POSTAGE_ADDR,
        &abi::PriceUpdate {
            price: U256::from(1u64),
        },
    );

    for (i, id) in [(1u64, batch_a), (2u64, batch_b)].into_iter() {
        apply(
            &indexer,
            100,
            i,
            POSTAGE_ADDR,
            &abi::BatchCreated {
                batchId: id,
                totalAmount: U256::from(1000u64),
                normalisedBalance: U256::from(1000u64), // IDENTICAL balance
                owner: address!("00000000000000000000000000000000000000aa"),
                depth: 20,
                bucketDepth: 16,
                immutableFlag: false,
            },
        );
    }

    let mut candidates = postage::eviction_candidates(&*db, 10).unwrap();
    candidates.sort();
    let mut expected = vec![batch_a, batch_b];
    expected.sort();
    assert_eq!(
        candidates, expected,
        "both equal-balance batches must remain in the eviction hint"
    );

    // And the typed projection still holds both rows.
    assert!(postage::batch_state(&*db, batch_a).unwrap().is_some());
    assert!(postage::batch_state(&*db, batch_b).unwrap().is_some());
}

#[test]
fn eviction_hint_orders_by_balance_ascending() {
    // The soonest-to-expire (lowest balance) batch heads the hint; the batchId
    // tie-break never reorders across distinct balances.
    let (db, indexer) = harness();
    let low = B256::repeat_byte(0x01);
    let high = B256::repeat_byte(0x02);
    // Insert high-balance first to prove the index sorts, not insertion order.
    apply(
        &indexer,
        1,
        0,
        POSTAGE_ADDR,
        &abi::BatchCreated {
            batchId: high,
            totalAmount: U256::from(9000u64),
            normalisedBalance: U256::from(9000u64),
            owner: Address::ZERO,
            depth: 20,
            bucketDepth: 16,
            immutableFlag: false,
        },
    );
    apply(
        &indexer,
        1,
        1,
        POSTAGE_ADDR,
        &abi::BatchCreated {
            batchId: low,
            totalAmount: U256::from(10u64),
            normalisedBalance: U256::from(10u64),
            owner: Address::ZERO,
            depth: 20,
            bucketDepth: 16,
            immutableFlag: false,
        },
    );
    let candidates = postage::eviction_candidates(&*db, 10).unwrap();
    assert_eq!(
        candidates,
        vec![low, high],
        "the lowest-balance batch must head the eviction hint"
    );
}

#[test]
fn no_price_update_means_validity_unknown() {
    // Fail-safe: with a batch indexed but NO PriceUpdate folded, validity is
    // not-yet-known (None), never a misleading Some(true). This is what turns a
    // wrong-address / wrong-ABI failure into a safe "unknown" rather than
    // "always valid".
    let (db, indexer) = harness();
    let batch_id = B256::repeat_byte(0xCD);
    apply(
        &indexer,
        50,
        0,
        POSTAGE_ADDR,
        &abi::BatchCreated {
            batchId: batch_id,
            totalAmount: U256::from(1000u64),
            normalisedBalance: U256::from(1000u64),
            owner: Address::ZERO,
            depth: 20,
            bucketDepth: 16,
            immutableFlag: false,
        },
    );
    assert_eq!(
        postage::is_batch_valid_now(&*db, batch_id, 60).unwrap(),
        None,
        "validity must be unknown until a PriceUpdate is indexed"
    );
    assert!(
        postage::chain_state(&*db).unwrap().is_none(),
        "chain_state must be None before any PriceUpdate"
    );
}

#[test]
fn revert_rebuilds_batch_projection_for_in_range_mutation() {
    // Regression: a batch created BEFORE from_block but topped up WITHIN the
    // reverted range must have its projection (and index) rebuilt to the
    // pre-revert balance, not left stale. revert rebuilds the whole postage
    // projection from surviving rows.
    let (db, indexer) = harness();
    let batch_id = B256::repeat_byte(0xEF);
    apply(
        &indexer,
        50,
        0,
        POSTAGE_ADDR,
        &abi::BatchCreated {
            batchId: batch_id,
            totalAmount: U256::from(1000u64),
            normalisedBalance: U256::from(1000u64),
            owner: Address::ZERO,
            depth: 20,
            bucketDepth: 16,
            immutableFlag: false,
        },
    );
    // Topup at block 150 raises the balance to 6000.
    apply(
        &indexer,
        150,
        0,
        POSTAGE_ADDR,
        &abi::BatchTopUp {
            batchId: batch_id,
            topupAmount: U256::from(5000u64),
            normalisedBalance: U256::from(6000u64),
        },
    );
    assert_eq!(
        postage::batch_state(&*db, batch_id)
            .unwrap()
            .unwrap()
            .normalised_balance,
        U256::from(6000u64)
    );

    // Revert from block 100: the topup (block 150) is dropped; the batch survives
    // (created block 50) with its ORIGINAL balance, and the index follows.
    indexer.revert(100).expect("revert");
    assert_eq!(
        postage::batch_state(&*db, batch_id)
            .unwrap()
            .unwrap()
            .normalised_balance,
        U256::from(1000u64),
        "the in-range topup must be reverted, not left stale in the projection"
    );
    let candidates = postage::eviction_candidates(&*db, 10).unwrap();
    assert_eq!(candidates, vec![batch_id]);
}

#[test]
fn staking_last_write_wins_and_overlay_inversion() {
    let (db, indexer) = harness();
    let owner = address!("00000000000000000000000000000000000000b1");
    let overlay1 = B256::repeat_byte(0x11);
    let overlay2 = B256::repeat_byte(0x22);

    apply(
        &indexer,
        10,
        0,
        STAKING_ADDR,
        &nectar_contracts::IStakeRegistry::StakeUpdated {
            owner,
            committedStake: U256::from(100u64),
            potentialStake: U256::from(200u64),
            overlay: overlay1,
            lastUpdatedBlock: U256::from(10u64),
            height: 4,
        },
    );
    assert!(staking::is_overlay_staked(&*db, overlay1).unwrap());
    assert_eq!(
        staking::owner_of_overlay(&*db, overlay1).unwrap(),
        Some(owner)
    );

    // Overlay changes: the new overlay is staked, the old is not.
    apply(
        &indexer,
        20,
        0,
        STAKING_ADDR,
        &abi::OverlayChanged {
            owner,
            overlay: overlay2,
        },
    );
    assert!(staking::is_overlay_staked(&*db, overlay2).unwrap());
    assert!(!staking::is_overlay_staked(&*db, overlay1).unwrap());

    // A slash zeroes both legs: the owner drops out of the set.
    apply(
        &indexer,
        30,
        0,
        STAKING_ADDR,
        &nectar_contracts::IStakeRegistry::StakeSlashed {
            slashed: owner,
            overlay: overlay2,
            amount: U256::from(300u64),
        },
    );
    assert!(!staking::is_overlay_staked(&*db, overlay2).unwrap());
    assert!(!staking::stake_of(&*db, owner).unwrap().unwrap().is_staked());
}

#[test]
fn chequebook_membership() {
    let (db, indexer) = harness();
    let cb = address!("00000000000000000000000000000000000000c1");
    assert!(!chequebook::is_factory_deployed(&*db, cb).unwrap());
    apply(
        &indexer,
        7,
        0,
        CHEQUEBOOK_ADDR,
        &nectar_contracts::IChequebookFactory::SimpleSwapDeployed {
            contractAddress: cb,
        },
    );
    assert!(chequebook::is_factory_deployed(&*db, cb).unwrap());
}

#[test]
fn swap_two_scalars_last_write_wins() {
    let (db, indexer) = harness();
    assert_eq!(swap::exchange_rate(&*db).unwrap(), None);

    apply(
        &indexer,
        1,
        0,
        SWAP_ADDR,
        &nectar_contracts::ISwapPriceOracle::PriceUpdate {
            price: U256::from(10u64),
        },
    );
    apply(
        &indexer,
        2,
        0,
        SWAP_ADDR,
        &nectar_contracts::ISwapPriceOracle::PriceUpdate {
            price: U256::from(20u64),
        },
    );
    apply(
        &indexer,
        3,
        0,
        SWAP_ADDR,
        &nectar_contracts::ISwapPriceOracle::ChequeValueDeductionUpdate {
            chequeValueDeduction: U256::from(5u64),
        },
    );
    assert_eq!(swap::exchange_rate(&*db).unwrap(), Some(U256::from(20u64)));
    assert_eq!(
        swap::cheque_value_deduction(&*db).unwrap(),
        Some(U256::from(5u64))
    );
}

#[test]
fn redistribution_groups_by_raw_round() {
    let (db, indexer) = harness();
    let overlay = B256::repeat_byte(0x33);

    apply(
        &indexer,
        1,
        0,
        REDIST_ADDR,
        &abi::Committed {
            roundNumber: U256::from(7u64),
            overlay,
            height: 4,
        },
    );
    apply(
        &indexer,
        1,
        1,
        REDIST_ADDR,
        &abi::Revealed {
            roundNumber: U256::from(7u64),
            overlay,
            stake: U256::from(1u64),
            stakeDensity: U256::from(2u64),
            reserveCommitment: B256::repeat_byte(0x44),
            depth: 4,
        },
    );
    apply(
        &indexer,
        2,
        0,
        REDIST_ADDR,
        &abi::CurrentRevealAnchor {
            roundNumber: U256::from(7u64),
            anchor: B256::repeat_byte(0x55),
        },
    );

    let round = redistribution::round(&*db, 7).unwrap().expect("round 7");
    assert_eq!(round.round, U256::from(7u64));
    assert_eq!(round.commits.len(), 1);
    assert_eq!(round.reveals.len(), 1);
    assert_eq!(round.anchor, Some(B256::repeat_byte(0x55)));

    // A different round does not collide.
    assert!(redistribution::round(&*db, 8).unwrap().is_none());
}

#[test]
fn revert_range_deletes_per_contract() {
    let (db, indexer) = harness();
    apply(
        &indexer,
        100,
        0,
        SWAP_ADDR,
        &nectar_contracts::ISwapPriceOracle::PriceUpdate {
            price: U256::from(1u64),
        },
    );
    apply(
        &indexer,
        200,
        0,
        SWAP_ADDR,
        &nectar_contracts::ISwapPriceOracle::PriceUpdate {
            price: U256::from(2u64),
        },
    );
    assert_eq!(swap::exchange_rate(&*db).unwrap(), Some(U256::from(2u64)));

    // Revert from block 150: the block-200 update is dropped, the block-100 one
    // survives.
    indexer.revert(150).expect("revert");
    assert_eq!(swap::exchange_rate(&*db).unwrap(), Some(U256::from(1u64)));
}

#[test]
fn descriptor_topic0s_are_the_signature_hashes() {
    // Sanity: each descriptor's topic0 is the event's SIGNATURE_HASH, the
    // contract-confusion authority.
    let postage = test_contracts()
        .into_iter()
        .find(|c| c.id == ContractId::Postage)
        .unwrap();
    let want = EventDescriptor {
        topic0: abi::BatchCreated::SIGNATURE_HASH,
        name: "BatchCreated",
    };
    assert!(postage.events.iter().any(|e| e.topic0 == want.topic0));
}

// Keep the `views` module path referenced so an unused-import lint never fires
// if a sub-view's test is the only consumer.
#[allow(unused_imports)]
use views as _views;
