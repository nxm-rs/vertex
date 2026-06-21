//! Redistribution + staking domain tests: the lazy per-round fold, the per-owner
//! stake fold with overlay inversion, ABI-signature pins, and the production
//! registration composing cleanly.

use std::sync::Arc;

use alloy_primitives::{Address, B256, LogData, U256, address, keccak256};
use alloy_rpc_types_eth::Log;
use alloy_sol_types::SolEvent;
use vertex_chain_index::Indexer;
use vertex_chain_index_framework::{
    ContractIndexer, DomainRegistration, EventDescriptor, Network, WatchedContract,
};
use vertex_storage_redb::RedbDatabase;

use crate::index::events;
use crate::index::{TAG_REDISTRIBUTION, TAG_STAKING, redistribution, staking};

// Synthetic addresses, distinct from any real deployment.
const REDIST_ADDR: Address = address!("0000000000000000000000000000000000000a03");
const STAKING_ADDR: Address = address!("0000000000000000000000000000000000000a02");

const REDISTRIBUTION_EVENTS: &[EventDescriptor] = &[
    EventDescriptor {
        topic0: events::Committed::SIGNATURE_HASH,
        name: "Committed",
    },
    EventDescriptor {
        topic0: events::Revealed::SIGNATURE_HASH,
        name: "Revealed",
    },
    EventDescriptor {
        topic0: events::CurrentRevealAnchor::SIGNATURE_HASH,
        name: "CurrentRevealAnchor",
    },
];

const STAKING_EVENTS: &[EventDescriptor] = &[
    EventDescriptor {
        topic0: nectar_contracts::IStakeRegistry::StakeUpdated::SIGNATURE_HASH,
        name: "StakeUpdated",
    },
    EventDescriptor {
        topic0: nectar_contracts::IStakeRegistry::StakeFrozen::SIGNATURE_HASH,
        name: "StakeFrozen",
    },
    EventDescriptor {
        topic0: nectar_contracts::IStakeRegistry::StakeSlashed::SIGNATURE_HASH,
        name: "StakeSlashed",
    },
    EventDescriptor {
        topic0: nectar_contracts::IStakeRegistry::StakeWithdrawn::SIGNATURE_HASH,
        name: "StakeWithdrawn",
    },
    EventDescriptor {
        topic0: events::OverlayChanged::SIGNATURE_HASH,
        name: "OverlayChanged",
    },
];

fn test_registration<DB: vertex_storage::Database>() -> DomainRegistration<DB> {
    DomainRegistration {
        contracts: vec![
            WatchedContract {
                tag: TAG_REDISTRIBUTION,
                address: REDIST_ADDR,
                start_block: 0,
                events: REDISTRIBUTION_EVENTS,
            },
            WatchedContract {
                tag: TAG_STAKING,
                address: STAKING_ADDR,
                start_block: 0,
                events: STAKING_EVENTS,
            },
        ],
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
    // The production registration with real canonical addresses composes cleanly.
    let db = Arc::new(RedbDatabase::in_memory().expect("db"));
    let indexer =
        ContractIndexer::from_registrations(db, vec![crate::index::registration(Network::Mainnet)])
            .expect("redistribution + staking registration must compose into the unified indexer");
    let filter = indexer.filter();
    assert_eq!(
        filter.address.len(),
        2,
        "redistribution + staking addresses"
    );
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
        &events::OverlayChanged {
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
fn redistribution_groups_by_raw_round() {
    let (db, indexer) = harness();
    let overlay = B256::repeat_byte(0x33);

    apply(
        &indexer,
        1,
        0,
        REDIST_ADDR,
        &events::Committed {
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
        &events::Revealed {
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
        &events::CurrentRevealAnchor {
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

/// The local event ABIs must match the canonical on-chain signatures
/// byte-for-byte; drift in field types or order changes the hash and breaks
/// decode-on-read.
#[test]
fn abi_signatures_match_canonical() {
    fn sig(s: &str) -> B256 {
        keccak256(s.as_bytes())
    }

    // StakeRegistry: the one event nectar lacks.
    assert_eq!(
        events::OverlayChanged::SIGNATURE_HASH,
        sig("OverlayChanged(address,bytes32)")
    );

    // Redistribution.
    assert_eq!(
        events::Committed::SIGNATURE_HASH,
        sig("Committed(uint256,bytes32,uint8)")
    );
    assert_eq!(
        events::Revealed::SIGNATURE_HASH,
        sig("Revealed(uint256,bytes32,uint256,uint256,bytes32,uint8)")
    );
    assert_eq!(
        events::CurrentRevealAnchor::SIGNATURE_HASH,
        sig("CurrentRevealAnchor(uint256,bytes32)")
    );
}

/// The production registration must watch the locally-pinned addresses, not the
/// diverging nectar ones (see FIXME(#315)).
#[test]
fn canonical_addresses() {
    fn watched(
        network: Network,
        tag: vertex_chain_index_framework::ContractTag,
    ) -> WatchedContract {
        crate::index::registration::<RedbDatabase>(network)
            .contracts
            .into_iter()
            .find(|c| c.tag == tag)
            .expect("contract in registration")
    }

    // Mainnet.
    let m_stake = watched(Network::Mainnet, TAG_STAKING);
    assert_eq!(
        m_stake.address,
        address!("da2a16EE889E7F04980A8d597b48c8D51B9518F4")
    );
    assert_eq!(m_stake.start_block, 40_430_237);
    let m_redist = watched(Network::Mainnet, TAG_REDISTRIBUTION);
    assert_eq!(
        m_redist.address,
        address!("5069cdfB3D9E56d23B1cAeE83CE6109A7E4fd62d")
    );
    assert_eq!(m_redist.start_block, 41_105_199);

    // Testnet.
    assert_eq!(
        watched(Network::Testnet, TAG_STAKING).address,
        address!("EEF13Ef9eD9cDD169701eeF3cd832df298dD1bB4")
    );
    assert_eq!(
        watched(Network::Testnet, TAG_REDISTRIBUTION).address,
        address!("5b718E36F5Ce2F2F7e25A397040436Ce6af3e89e")
    );
}
