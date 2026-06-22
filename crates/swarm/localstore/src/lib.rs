//! Swarm local store: the client chunk cache and its configuration.

#[cfg(feature = "cli")]
mod args;
mod backend;
mod chunk_store;
mod config;

#[cfg(feature = "cli")]
pub use args::LocalStoreArgs;
pub use backend::{CacheBackend, LruBackend};
pub use chunk_store::{CacheValue, ChunkStore, Clock, SystemClock};
pub use config::{DEFAULT_CACHE_BUDGET_BYTES, DEFAULT_SOC_CACHE_TTL_NS, LocalStoreConfig};

#[cfg(all(feature = "indexeddb", target_arch = "wasm32"))]
pub use backend::IndexedDbBackend;
