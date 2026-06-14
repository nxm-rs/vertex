//! Generic, reorg-safe, contract-agnostic chain event-indexing engine.
//!
//! This crate is the foundation per-contract indexers build on. It owns the
//! reorg, cursor, and paging story once, so a per-contract indexer (postage
//! batches, the price oracles, the chequebook factory, staking, redistribution)
//! only declares what to watch and how to fold one log.
//!
//! # Shape
//!
//! - [`Indexer`]: the minimalist, contract-specific trait. Declare a start
//!   block and a log [`Filter`](alloy_rpc_types_eth::Filter), and fold one
//!   decoded log in [`apply`](Indexer::apply).
//! - [`EventEngine`]: the driver. One instance backfills a single indexer from
//!   its deployment block (or persisted cursor) to the chain's finalized head,
//!   then follows the head, indexing each newly-finalized range the same way.
//! - [`Cursor`]: the per-indexer checkpoint persisted in `vertex-storage`,
//!   keyed by the indexer's [`name`](Indexer::name).
//! - [`ChainReader`]: the narrow chain-read slice the engine drives, blanket
//!   implemented for every `alloy_provider::Provider<Ethereum>`.
//! - [`IndexError`]: the typed error taxonomy, with `strum::IntoStaticStr`
//!   `reason` metric labels matching the `vertex-chain` `ChainError` style.
//!
//! # Reorg strategy
//!
//! The engine indexes only up to the chain's `finalized` tag, not a fixed block
//! lag. Finalized blocks do not reorg, so the indexed range is canonical by
//! construction and needs no rollback logic; the cost is one finality window of
//! latency. Optimistic head-tracking (indexing `finalized..latest` and reverting
//! on the WS `log.removed` flag or a parent-hash mismatch) is a documented
//! enhancement, not implemented: the hooks ([`Indexer::revert`], the block hash
//! in the [`Cursor`]) exist, but the MVP never calls `revert`.
//!
//! # Placement
//!
//! Native, RPC-and-persistence code; intended to run behind a `chain` feature in
//! its consumers and to stay out of the wasm cone. The crate itself depends only
//! on `alloy` with `default-features = false` (no reqwest, no native TLS) and on
//! the `vertex-storage` trait surface, so it imposes no wasm-hostile transport
//! of its own; the consumer selects the transport and the storage backend.
//!
//! # Usage sketch
//!
//! ```ignore
//! let engine = EventEngine::new(provider, db).register(my_indexer);
//! engine.run(shutdown).await?;
//! ```

mod cursor;
mod engine;
mod error;
mod indexer;
mod metrics;
mod notify;
mod reader;

#[cfg(test)]
mod tests;

pub use cursor::{Cursor, CursorTable, CursorTables};
pub use engine::{DEFAULT_PAGE_SIZE, DEFAULT_POLL_INTERVAL, EventEngine, EventEngineBuilder};
pub use error::IndexError;
pub use indexer::Indexer;
pub use notify::{BlockTip, BlockTipRx, IndexAdvance, IndexAdvanceRx};
pub use reader::{ChainReader, FinalizedHead};
