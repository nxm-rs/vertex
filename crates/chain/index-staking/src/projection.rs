//! The staking projection: per-owner stake state and the staked-overlay set.
//!
//! Two `vertex-storage` tables hold the folded state:
//!
//! - [`StakeTable`] maps an owner [`Address`] to its [`OwnerStake`] row: the
//!   committed and potential stake, the current overlay and height, the block the
//!   stake was last updated on-chain, and the freeze deadline. This is the
//!   authoritative per-owner projection.
//! - [`OverlayOwnerTable`] maps a staked `overlay` to the owner that staked it.
//!   It is the staked-overlay set, kept queryable by overlay so a consumer can
//!   resolve "who staked this overlay" without scanning every owner. The fold
//!   maintains it in lockstep with the overlay field on the owner row: when an
//!   owner's overlay changes, the stale overlay key is removed and the new one
//!   inserted, so the set never carries a dangling overlay.
//!
//! Both tables are written through the same projection write transaction, so an
//! owner row and its overlay-set entry commit atomically. Reads go through
//! [`StakeProjection`], a thin query surface over the two tables.

use alloy_primitives::{Address, B256, U256};
use serde::{Deserialize, Serialize};
use vertex_storage::{Database, DatabaseError, DbTx, DbTxMut, Table, Tables};

// Owner address -> folded stake state.
vertex_storage::table!(pub StakeTable, "staking_stakes", Address, OwnerStake);

// Staked overlay -> the owner that staked it (the staked-overlay set).
vertex_storage::table!(
    pub OverlayOwnerTable,
    "staking_overlay_owners",
    OverlayKey,
    Address
);

/// The overlay-set table key: a 32-byte overlay.
///
/// A newtype over [`B256`] so the overlay satisfies the storage key contract
/// (`Encode`/`Decode`/`Ord`) and orders lexicographically by its bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OverlayKey(pub B256);

impl From<B256> for OverlayKey {
    fn from(overlay: B256) -> Self {
        Self(overlay)
    }
}

impl vertex_storage::Encode for OverlayKey {
    type Encoded = [u8; 32];

    fn encode(self) -> Self::Encoded {
        self.0.into()
    }
}

impl vertex_storage::Decode for OverlayKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let bytes: [u8; 32] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        Ok(Self(B256::from(bytes)))
    }
}

/// The set of tables the staking projection persists, for one-shot init.
pub struct StakingTables;

impl Tables for StakingTables {
    const NAMES: &'static [&'static str] = &[StakeTable::NAME, OverlayOwnerTable::NAME];
}

/// One owner's folded stake state.
///
/// Every field is the last value the chain reported for this owner. `committed`
/// and `potential` are the two stake legs from `StakeUpdated`; `overlay` and
/// `height` pin the owner's current neighbourhood; `last_updated_block` is the
/// on-chain block the stake was last changed (the contract's own counter, not
/// the log's block); `frozen_until` is the freeze deadline (`0` when not frozen).
///
/// `seen_at` records the `(block, log_index)` of the log that produced this row,
/// so the fold can reject an out-of-order replay: a later supersede must come
/// from a strictly later log position. Because the engine delivers finalized
/// logs in order and re-delivers the same range verbatim on restart, this makes
/// the fold idempotent and monotonic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    /// The `(block, log_index)` of the log that produced this row.
    pub seen_at: LogPos,
}

/// A log's position in the canonical order: `(block_number, log_index)`.
///
/// The fold supersedes a row only with a strictly later position, so a replayed
/// log (same position) is a no-op and an out-of-order log never regresses state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LogPos {
    /// The log's block number.
    pub block: u64,
    /// The log's index within the block.
    pub index: u64,
}

impl OwnerStake {
    /// A fresh, never-seen owner row: a sentinel earlier than any real log.
    ///
    /// Used as the base for the first event touching an owner. Its `seen_at` is
    /// the minimum position, so the first real log always supersedes it.
    fn empty() -> Self {
        Self {
            committed: U256::ZERO,
            potential: U256::ZERO,
            overlay: B256::ZERO,
            height: 0,
            last_updated_block: U256::ZERO,
            frozen_until: U256::ZERO,
            seen_at: LogPos { block: 0, index: 0 },
        }
    }

    /// Whether this owner currently holds a non-zero stake.
    pub fn is_staked(&self) -> bool {
        !self.potential.is_zero() || !self.committed.is_zero()
    }

    /// Whether this owner is frozen at `block` (the freeze deadline is in the
    /// future relative to the supplied block height).
    pub fn is_frozen_at(&self, block: U256) -> bool {
        self.frozen_until > block
    }
}

/// A read-only query surface over the staking projection tables.
///
/// Wraps a `vertex-storage` [`Database`] and answers the questions a consumer
/// asks of the staking projection at its decision point: an owner's stake row,
/// whether an overlay is in the staked set, and the owner behind a staked
/// overlay. Per `CHAIN_REACTIONS_DESIGN.md`, reactions are lazy: a consumer reads
/// this projection when it decides, it is never pushed a stake event.
pub struct StakeProjection<'a, DB> {
    db: &'a DB,
}

impl<'a, DB: Database> StakeProjection<'a, DB> {
    /// Wrap a database for staking-projection reads.
    pub fn new(db: &'a DB) -> Self {
        Self { db }
    }

    /// The folded stake row for `owner`, if the owner has ever been seen.
    pub fn stake_of(&self, owner: Address) -> Result<Option<OwnerStake>, DatabaseError> {
        self.db.view(|tx| tx.get::<StakeTable>(owner))
    }

    /// Whether `overlay` is currently in the staked-overlay set.
    pub fn is_overlay_staked(&self, overlay: B256) -> Result<bool, DatabaseError> {
        Ok(self.owner_of_overlay(overlay)?.is_some())
    }

    /// The owner behind a staked `overlay`, if the overlay is in the set.
    pub fn owner_of_overlay(&self, overlay: B256) -> Result<Option<Address>, DatabaseError> {
        self.db
            .view(|tx| tx.get::<OverlayOwnerTable>(OverlayKey::from(overlay)))
    }

    /// Every staked overlay paired with its owner.
    pub fn staked_overlays(&self) -> Result<Vec<(B256, Address)>, DatabaseError> {
        self.db.view(|tx| {
            Ok(tx
                .entries::<OverlayOwnerTable>()?
                .into_iter()
                .map(|(k, owner)| (k.0, owner))
                .collect())
        })
    }
}

/// Apply a fold step against an owner's row, within a write transaction.
///
/// Loads the current row (or the empty sentinel), runs `update` to produce the
/// next row, then reconciles the staked-overlay set so it reflects the new row's
/// membership: the set carries `(overlay -> owner)` exactly when the owner holds
/// a non-zero stake under a non-zero overlay. The previous set entry is removed
/// whenever the owner's overlay or staked status changed, and the new entry is
/// inserted whenever the owner is staked. The row and the set entry are written
/// through the same transaction, so they commit atomically.
///
/// `update` returns `None` to skip the write entirely (a stale/replayed log that
/// must not supersede), keeping the fold idempotent.
pub(crate) fn fold_owner<TX, F>(tx: &TX, owner: Address, update: F) -> Result<(), DatabaseError>
where
    TX: DbTxMut,
    F: FnOnce(OwnerStake) -> Option<OwnerStake>,
{
    let current = tx
        .get::<StakeTable>(owner)?
        .unwrap_or_else(OwnerStake::empty);
    let prev_in_set = current.is_staked() && current.overlay != B256::ZERO;
    let prev_overlay = current.overlay;

    let Some(next) = update(current) else {
        return Ok(());
    };

    let next_in_set = next.is_staked() && next.overlay != B256::ZERO;

    // Remove the old set entry when the owner was in the set and either its
    // overlay moved or it is leaving the set (slash/withdraw zeroed the stake).
    // Keying the delete on the previous overlay leaves an unrelated owner that
    // happens to share the new overlay untouched.
    if prev_in_set && (!next_in_set || next.overlay != prev_overlay) {
        tx.delete::<OverlayOwnerTable>(OverlayKey::from(prev_overlay))?;
    }
    // Insert (or refresh) the set entry whenever the owner is staked.
    if next_in_set {
        tx.put::<OverlayOwnerTable>(OverlayKey::from(next.overlay), owner)?;
    }

    tx.put::<StakeTable>(owner, next)?;
    Ok(())
}
