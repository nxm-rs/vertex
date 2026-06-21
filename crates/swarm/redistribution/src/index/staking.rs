//! Staking view: per-owner stake and the staked-overlay set by lazy fold over
//! the StakeRegistry rows under [`TAG_STAKING`], last-write-wins in canonical
//! order. Overlay -> owner is a read-time inversion. No reducer, no projection
//! table.

use std::collections::HashMap;

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;
use nectar_contracts::IStakeRegistry;
use vertex_chain_index_framework::fold_events;
use vertex_storage::{Database, DatabaseError};

use crate::index::TAG_STAKING;
use crate::index::events;

/// One owner's folded stake state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OwnerStake {
    /// The committed (locked) stake leg.
    pub committed: U256,
    /// The potential (effective) stake leg.
    pub potential: U256,
    /// The owner's current overlay.
    pub overlay: B256,
    /// The owner's current neighbourhood height.
    pub height: u8,
    /// The on-chain block the stake was last updated (the contract's counter).
    pub last_updated_block: U256,
    /// The freeze deadline; `0` means not frozen.
    pub frozen_until: U256,
}

impl OwnerStake {
    /// Whether this owner currently holds a non-zero stake.
    pub fn is_staked(&self) -> bool {
        !self.potential.is_zero() || !self.committed.is_zero()
    }

    /// Whether this owner is frozen at `block`.
    pub fn is_frozen_at(&self, block: U256) -> bool {
        self.frozen_until > block
    }
}

/// Fold the staking rows into a per-owner map; every staking read narrows this.
///
/// Canonical position order makes last-write-wins implicit. Cost is O(total
/// staking history) per call, with no caching; not for a per-request path.
fn fold_owners<DB: Database>(db: &DB) -> Result<HashMap<Address, OwnerStake>, DatabaseError> {
    fold_events(
        db,
        TAG_STAKING,
        HashMap::<Address, OwnerStake>::new(),
        |owners, _key, ev| {
            let data = ev.log_data();
            if ev.topic0 == IStakeRegistry::StakeUpdated::SIGNATURE_HASH
                && let Ok(e) = IStakeRegistry::StakeUpdated::decode_log_data(&data)
            {
                let s = owners.entry(e.owner).or_default();
                s.committed = e.committedStake;
                s.potential = e.potentialStake;
                s.overlay = e.overlay;
                s.height = e.height;
                s.last_updated_block = e.lastUpdatedBlock;
            } else if ev.topic0 == IStakeRegistry::StakeFrozen::SIGNATURE_HASH
                && let Ok(e) = IStakeRegistry::StakeFrozen::decode_log_data(&data)
            {
                owners.entry(e.frozen).or_default().frozen_until = e.time;
            } else if ev.topic0 == IStakeRegistry::StakeSlashed::SIGNATURE_HASH
                && let Ok(e) = IStakeRegistry::StakeSlashed::decode_log_data(&data)
            {
                let s = owners.entry(e.slashed).or_default();
                s.committed = U256::ZERO;
                s.potential = U256::ZERO;
            } else if ev.topic0 == IStakeRegistry::StakeWithdrawn::SIGNATURE_HASH
                && let Ok(e) = IStakeRegistry::StakeWithdrawn::decode_log_data(&data)
            {
                let s = owners.entry(e.node).or_default();
                s.committed = U256::ZERO;
                s.potential = U256::ZERO;
            } else if ev.topic0 == events::OverlayChanged::SIGNATURE_HASH
                && let Ok(e) = events::OverlayChanged::decode_log_data(&data)
            {
                owners.entry(e.owner).or_default().overlay = e.overlay;
            }
        },
    )
}

/// The folded stake row for `owner`, if the owner has ever been seen.
pub fn stake_of<DB: Database>(
    db: &DB,
    owner: Address,
) -> Result<Option<OwnerStake>, DatabaseError> {
    Ok(fold_owners(db)?.get(&owner).copied())
}

/// Whether `overlay` is currently in the staked-overlay set.
pub fn is_overlay_staked<DB: Database>(db: &DB, overlay: B256) -> Result<bool, DatabaseError> {
    Ok(owner_of_overlay(db, overlay)?.is_some())
}

/// The owner behind a staked `overlay`, if any owner currently stakes it.
pub fn owner_of_overlay<DB: Database>(
    db: &DB,
    overlay: B256,
) -> Result<Option<Address>, DatabaseError> {
    let owners = fold_owners(db)?;
    Ok(owners.into_iter().find_map(|(owner, s)| {
        (s.is_staked() && s.overlay != B256::ZERO && s.overlay == overlay).then_some(owner)
    }))
}

/// Every staked overlay paired with its owner.
pub fn staked_overlays<DB: Database>(db: &DB) -> Result<Vec<(B256, Address)>, DatabaseError> {
    let owners = fold_owners(db)?;
    Ok(owners
        .into_iter()
        .filter(|(_, s)| s.is_staked() && s.overlay != B256::ZERO)
        .map(|(owner, s)| (s.overlay, owner))
        .collect())
}
