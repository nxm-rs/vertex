//! Swarm local store: the client chunk cache and its configuration.

#[cfg(feature = "cli")]
mod args;
mod chunk_store;
mod config;

#[cfg(feature = "cli")]
pub use args::LocalStoreArgs;
pub use chunk_store::{ChunkStore, Clock, SystemClock};
pub use config::{DEFAULT_CACHE_BUDGET_BYTES, DEFAULT_SOC_CACHE_TTL_NS, LocalStoreConfig};
