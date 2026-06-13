//! PostageStamp contract event indexer: a pure, idempotent projection of the
//! Swarm postage batch set, the storage price, and the contract chain-state.
//!
//! The PostageStamp contract on Gnosis Chain manages the postage batches that
//! authorise uploads to Swarm. This crate plugs a [`PostageIndexer`] into the
//! generic [`EventEngine`](vertex_chain_index::EventEngine) and folds the
//! contract's logs into a `vertex-storage` projection a consumer reads lazily.
//!
//! # What it indexes
//!
//! Five events from the contract ABI:
//!
//! - `BatchCreated`, `BatchTopUp`, `BatchDepthIncrease` maintain the **batch
//!   set**: per-batch owner, depth, bucket depth, `normalisedBalance`,
//!   immutability, and creation block.
//! - `PriceUpdate` drives the **pricing chain-state**: the current per-chunk
//!   per-block price and the running `totalOutPayment` accumulator, reconstructed
//!   from the price-update cadence exactly as the contract maintains it.
//! - `Paused` records the contract pause flag.
//!
//! # The projection
//!
//! - [`BatchTable`](projection::BatchTable): the batch set, keyed by `batchId`.
//! - [`ChainStateTable`](projection::ChainStateTable): the single-row pricing
//!   chain-state (`totalOutPayment`, `lastPrice`, `lastUpdatedBlock`, `paused`).
//!
//! # The validity query
//!
//! The headline read helper,
//! [`is_batch_valid_now`](projection::is_batch_valid_now), answers "is this batch
//! valid at this block" as `normalisedBalance > currentTotalOutPayment(block)`,
//! where `currentTotalOutPayment(block) = totalOutPayment + lastPrice * (block -
//! lastUpdatedBlock)`. It is a pure read against the projection plus the live
//! block clock.
//!
//! # Pure, idempotent fold (no reactions)
//!
//! Per `CHAIN_REACTIONS_DESIGN.md`,
//! [`PostageIndexer::apply`](vertex_chain_index::Indexer::apply) is a pure fold:
//! it writes only the projection tables, never calls into the storer, accounting,
//! the postage issuer, or any other domain, and re-applying a finalized log
//! re-applies to the same row. It performs **no eviction and no reaction**: batch
//! expiry has no event to hook (a batch dies when the rising
//! `currentTotalOutPayment` line crosses its static `normalisedBalance`, with no
//! transaction at the crossing), so a consumer recomputes validity lazily on the
//! block clock at its own decision point. The chain crate stays domain-agnostic.
//!
//! # Placement
//!
//! Native, RPC-and-persistence code intended to run behind a `chain` feature in
//! its consumers and to stay out of the wasm cone, like the engine it builds on.

pub mod events;
pub mod indexer;
pub mod projection;

#[cfg(test)]
mod tests;

pub use indexer::{
    INDEXER_NAME, POSTAGE_STAMP_ADDRESS, POSTAGE_STAMP_DEPLOYMENT_BLOCK, PostageIndexer,
};
pub use projection::{
    BatchKey, BatchState, BatchTable, ChainState, ChainStateKey, ChainStateTable, LogPosition,
    PostageTables, is_batch_valid_now, read_batch, read_chain_state,
};
