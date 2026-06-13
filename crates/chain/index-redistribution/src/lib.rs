//! Redistribution game event indexer: a pure, idempotent projection of the
//! Swarm storage-incentive round logs.
//!
//! The Redistribution contract runs the storage-incentive game on Gnosis Chain:
//! every round, nodes `Committed` an obfuscated hash, then `Revealed` their
//! reserve commitment, a truth is selected, and a winner is paid. This crate
//! plugs a [`RedistributionIndexer`] into the generic
//! [`EventEngine`](vertex_chain_index::EventEngine) and folds those logs into a
//! `vertex-storage` projection.
//!
//! # What it does, and pointedly does not do
//!
//! Per the chain-reactions design, the redistribution game is a **clock, not a
//! reaction source**: `phase = block % blocksPerRound`. This indexer only
//! RECORDS the round logs. [`Indexer::apply`](vertex_chain_index::Indexer::apply)
//! is a pure, idempotent fold into the projection: it writes rows and returns. It
//! calls into no storer, accounting, or redistribution agent; it fires no
//! reaction; it gates no node behaviour. A replayed finalized log re-writes the
//! same projection row to the same value. Any node logic that needs to know the
//! game state reads the projection lazily at its own decision point.
//!
//! # Projection
//!
//! - [`RoundTable`](projection::RoundTable): the per-round game state keyed by the
//!   on-chain `roundNumber`, folding the round-carrying events (`Committed`,
//!   `Revealed`, `CurrentRevealAnchor`).
//! - [`RoundEventTable`](projection::RoundEventTable): the raw event log keyed by
//!   `(block_number, log_index)`, recording every selected event verbatim,
//!   including the round-terminal ones whose payload does not name a round
//!   (`TruthSelected`, `WinnerSelected`, `ChunkCount`, `CountCommits`,
//!   `CountReveals`).
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
    INDEXER_NAME, REDISTRIBUTION_ADDRESS, REDISTRIBUTION_DEPLOYMENT_BLOCK, RedistributionIndexer,
};
pub use projection::{
    Commit, LogKey, RedistributionTables, Reveal, RoundEvent, RoundEventTable, RoundKey,
    RoundState, RoundTable,
};
