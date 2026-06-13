//! Unit tests over synthetic ABI-encoded `StakeRegistry` logs.
//!
//! Each test builds a real, ABI-encoded log with the contract's `sol!` event
//! types, folds it through [`StakingIndexer::apply`], and asserts the projection.
//! No chain or RPC is involved: the in-memory `vertex-storage` backend holds the
//! projection, and the logs are encoded exactly as the contract would emit them,
//! so decoding exercises the same path a live log would.

use std::sync::Arc;

use alloy_primitives::{Address, B256, LogData, U256};
use alloy_rpc_types_eth::Log;
use alloy_sol_types::SolEvent;
use nectar_contracts::IStakeRegistry;
use vertex_chain_index::Indexer;
use vertex_storage_redb::RedbDatabase;

use crate::indexer::{OverlayChanged, STAKE_REGISTRY, StakingIndexer};
use crate::projection::StakeProjection;

/// Wrap a `sol!` event's encoded log data in an `alloy_rpc_types_eth::Log` at the
/// given `(block, log_index)`, emitted by the registry address.
fn log_of<E: SolEvent>(event: &E, block: u64, index: u64) -> Log {
    let data: LogData = event.encode_log_data();
    Log {
        inner: alloy_primitives::Log {
            address: STAKE_REGISTRY,
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

fn updated(
    owner: Address,
    committed: u64,
    potential: u64,
    overlay: B256,
    last_updated: u64,
    height: u8,
) -> IStakeRegistry::StakeUpdated {
    IStakeRegistry::StakeUpdated {
        owner,
        committedStake: U256::from(committed),
        potentialStake: U256::from(potential),
        overlay,
        lastUpdatedBlock: U256::from(last_updated),
        height,
    }
}

fn indexer() -> (Arc<RedbDatabase>, StakingIndexer<RedbDatabase>) {
    let db = Arc::new(RedbDatabase::in_memory().unwrap());
    let idx = StakingIndexer::new(db.clone()).unwrap();
    (db, idx)
}

fn owner(byte: u8) -> Address {
    Address::repeat_byte(byte)
}

fn overlay(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}

#[test]
fn stake_updated_folds_all_fields() {
    let (db, idx) = indexer();
    let o = owner(0x11);
    let ov = overlay(0xaa);

    let log = log_of(&updated(o, 100, 250, ov, 40_500_000, 3), 40_500_001, 0);
    idx.apply(40_500_001, &log).unwrap();

    let proj = StakeProjection::new(db.as_ref());
    let row = proj.stake_of(o).unwrap().expect("owner row");
    assert_eq!(row.committed, U256::from(100u64));
    assert_eq!(row.potential, U256::from(250u64));
    assert_eq!(row.overlay, ov);
    assert_eq!(row.height, 3);
    assert_eq!(row.last_updated_block, U256::from(40_500_000u64));
    assert_eq!(row.frozen_until, U256::ZERO);
    assert!(row.is_staked());

    // The overlay joined the staked set, pointing back at the owner.
    assert!(proj.is_overlay_staked(ov).unwrap());
    assert_eq!(proj.owner_of_overlay(ov).unwrap(), Some(o));
    assert_eq!(proj.staked_overlays().unwrap(), vec![(ov, o)]);
}

#[test]
fn stake_frozen_records_deadline_without_clearing_stake() {
    let (db, idx) = indexer();
    let o = owner(0x22);
    let ov = overlay(0xbb);

    idx.apply(1, &log_of(&updated(o, 10, 20, ov, 100, 1), 100, 0))
        .unwrap();

    let frozen = IStakeRegistry::StakeFrozen {
        frozen: o,
        overlay: ov,
        time: U256::from(40_600_000u64),
    };
    idx.apply(1, &log_of(&frozen, 101, 0)).unwrap();

    let proj = StakeProjection::new(db.as_ref());
    let row = proj.stake_of(o).unwrap().unwrap();
    assert_eq!(row.frozen_until, U256::from(40_600_000u64));
    assert!(row.is_frozen_at(U256::from(40_599_999u64)));
    assert!(!row.is_frozen_at(U256::from(40_600_000u64)));
    // The stake and overlay survive a freeze.
    assert!(row.is_staked());
    assert!(proj.is_overlay_staked(ov).unwrap());
}

#[test]
fn stake_slashed_zeroes_stake_and_drops_overlay() {
    let (db, idx) = indexer();
    let o = owner(0x33);
    let ov = overlay(0xcc);

    idx.apply(1, &log_of(&updated(o, 10, 20, ov, 100, 1), 100, 0))
        .unwrap();
    assert!(
        StakeProjection::new(db.as_ref())
            .is_overlay_staked(ov)
            .unwrap()
    );

    let slashed = IStakeRegistry::StakeSlashed {
        slashed: o,
        overlay: ov,
        amount: U256::from(30u64),
    };
    idx.apply(1, &log_of(&slashed, 101, 0)).unwrap();

    let proj = StakeProjection::new(db.as_ref());
    let row = proj.stake_of(o).unwrap().unwrap();
    assert_eq!(row.committed, U256::ZERO);
    assert_eq!(row.potential, U256::ZERO);
    assert!(!row.is_staked());
    // A zeroed stake removes the owner from the staked-overlay set; the row
    // survives (with its overlay field intact) but the set no longer carries it.
    assert!(!proj.is_overlay_staked(ov).unwrap());
    assert_eq!(proj.owner_of_overlay(ov).unwrap(), None);
    assert!(proj.staked_overlays().unwrap().is_empty());
}

#[test]
fn stake_withdrawn_zeroes_stake() {
    let (db, idx) = indexer();
    let o = owner(0x44);
    let ov = overlay(0xdd);

    idx.apply(1, &log_of(&updated(o, 10, 20, ov, 100, 1), 100, 0))
        .unwrap();

    let withdrawn = IStakeRegistry::StakeWithdrawn {
        node: o,
        amount: U256::from(30u64),
    };
    idx.apply(1, &log_of(&withdrawn, 101, 0)).unwrap();

    let proj = StakeProjection::new(db.as_ref());
    let row = proj.stake_of(o).unwrap().unwrap();
    assert_eq!(row.potential, U256::ZERO);
    assert!(!row.is_staked());
    // Withdrawing the stake also drops the overlay from the staked set.
    assert!(!proj.is_overlay_staked(ov).unwrap());
}

#[test]
fn overlay_changed_repoints_the_set() {
    let (db, idx) = indexer();
    let o = owner(0x55);
    let ov1 = overlay(0x01);
    let ov2 = overlay(0x02);

    idx.apply(1, &log_of(&updated(o, 10, 20, ov1, 100, 1), 100, 0))
        .unwrap();

    let changed = OverlayChanged {
        owner: o,
        overlay: ov2,
    };
    idx.apply(1, &log_of(&changed, 101, 0)).unwrap();

    let proj = StakeProjection::new(db.as_ref());
    let row = proj.stake_of(o).unwrap().unwrap();
    assert_eq!(row.overlay, ov2);
    // The old overlay left the set; the new one took its place.
    assert!(!proj.is_overlay_staked(ov1).unwrap());
    assert!(proj.is_overlay_staked(ov2).unwrap());
    assert_eq!(proj.owner_of_overlay(ov2).unwrap(), Some(o));
    assert_eq!(proj.staked_overlays().unwrap(), vec![(ov2, o)]);
}

#[test]
fn replaying_a_log_is_idempotent() {
    let (db, idx) = indexer();
    let o = owner(0x66);
    let ov = overlay(0xee);
    let log = log_of(&updated(o, 100, 250, ov, 40_500_000, 3), 40_500_001, 0);

    // Apply the identical finalized log twice; the second is a no-op, leaving the
    // row and the overlay set exactly as the first produced them.
    idx.apply(40_500_001, &log).unwrap();
    let first = StakeProjection::new(db.as_ref()).stake_of(o).unwrap();
    idx.apply(40_500_001, &log).unwrap();
    let second = StakeProjection::new(db.as_ref()).stake_of(o).unwrap();

    assert_eq!(
        first, second,
        "replaying a finalized log re-applies the same row"
    );
    assert_eq!(
        StakeProjection::new(db.as_ref())
            .staked_overlays()
            .unwrap()
            .len(),
        1,
        "replay does not duplicate the overlay-set entry"
    );
}

#[test]
fn supersede_is_monotonic_by_block_then_log_index() {
    let (db, idx) = indexer();
    let o = owner(0x77);
    let ov = overlay(0x07);

    // A later log (block 200) sets potential to 500.
    idx.apply(200, &log_of(&updated(o, 0, 500, ov, 200, 2), 200, 1))
        .unwrap();

    // An earlier log (block 100) must NOT regress the row, even though it is
    // delivered second here: the supersede key is (block, log_index).
    idx.apply(100, &log_of(&updated(o, 0, 10, ov, 100, 1), 100, 0))
        .unwrap();

    let row = StakeProjection::new(db.as_ref())
        .stake_of(o)
        .unwrap()
        .unwrap();
    assert_eq!(
        row.potential,
        U256::from(500u64),
        "an out-of-order earlier log does not overwrite a later one"
    );
    assert_eq!(row.height, 2);

    // A same-block log with a higher log_index does supersede.
    idx.apply(200, &log_of(&updated(o, 0, 999, ov, 200, 4), 200, 2))
        .unwrap();
    let row = StakeProjection::new(db.as_ref())
        .stake_of(o)
        .unwrap()
        .unwrap();
    assert_eq!(row.potential, U256::from(999u64));
    assert_eq!(row.height, 4);
}

#[test]
fn filter_targets_the_registry_and_the_five_events() {
    let (_db, idx) = indexer();
    let filter = idx.filter();

    // The address constraint is the registry.
    assert!(filter.address.iter().any(|a| *a == STAKE_REGISTRY));

    // topic0 carries exactly the five staking signatures.
    let topics = &filter.topics[0];
    let expected = [
        IStakeRegistry::StakeUpdated::SIGNATURE_HASH,
        IStakeRegistry::StakeFrozen::SIGNATURE_HASH,
        IStakeRegistry::StakeSlashed::SIGNATURE_HASH,
        IStakeRegistry::StakeWithdrawn::SIGNATURE_HASH,
        OverlayChanged::SIGNATURE_HASH,
    ];
    for sig in expected {
        assert!(topics.iter().any(|t| *t == sig), "filter matches {sig}");
    }
    assert_eq!(topics.iter().count(), expected.len());
}
