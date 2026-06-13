//! Staking registry chain indexer for Vertex.
//!
//! A per-contract [`Indexer`](vertex_chain_index::Indexer) for the Swarm
//! `StakeRegistry`, built on the generic [`vertex_chain_index`] engine. It folds
//! the registry's events into a [`vertex-storage`](vertex_storage) projection:
//! the per-owner stake state (committed and potential stake, overlay, height,
//! last-updated block, freeze deadline) and the staked-overlay set.
//!
//! # Events indexed
//!
//! - `StakeUpdated(owner, committedStake, potentialStake, overlay, lastUpdatedBlock, height)`
//!   sets an owner's two stake legs, overlay, height, and on-chain update block.
//! - `StakeFrozen(frozen, overlay, time)` records an owner's freeze deadline.
//! - `StakeSlashed(slashed, overlay, amount)` zeroes an owner's stake (removing
//!   it from the staked-overlay set).
//! - `StakeWithdrawn(node, amount)` zeroes an owner's stake on withdrawal.
//! - `OverlayChanged(owner, overlay)` re-points an owner's overlay.
//!
//! # Design: a pure, lazy projection
//!
//! Per `CHAIN_REACTIONS_DESIGN.md`, [`apply`](vertex_chain_index::Indexer::apply)
//! is a pure, idempotent fold into the storage projection: no side effects, no
//! domain logic, no calls into a storer or accounting layer. Consumers read the
//! projection lazily at their own decision point (for example, checking whether
//! an owner is frozen right before committing a redistribution round) through
//! [`StakeProjection`]; the chain crate never pushes a stake event at them.
//!
//! Idempotency and monotonicity come from a `(block, log_index)` supersede rule:
//! a fold step updates an owner's row only when the log's position is strictly
//! later than the position last recorded for that owner, so re-delivering a
//! finalized range on restart re-applies to the same row and an out-of-order log
//! never regresses state.
//!
//! # Placement
//!
//! Native, RPC-and-persistence code, intended to run behind a `chain` feature in
//! its consumers and to stay out of the wasm cone, exactly like the engine it
//! builds on. The crate decodes logs the engine already fetched and persists
//! through the `vertex-storage` trait surface; it opens no socket of its own.

mod indexer;
mod projection;

pub use indexer::{DEPLOYMENT_BLOCK, STAKE_REGISTRY, StakingIndexer};
pub use projection::{
    LogPos, OverlayKey, OverlayOwnerTable, OwnerStake, StakeProjection, StakeTable, StakingTables,
};

#[cfg(test)]
mod tests;
