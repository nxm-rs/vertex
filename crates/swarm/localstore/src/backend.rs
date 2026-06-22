//! The swappable byte-store backend under [`ChunkStore`](crate::ChunkStore).
//!
//! The [`SwarmLocalStore`](vertex_swarm_api::SwarmLocalStore) freshness and
//! last-write-wins policy lives once in `chunk_store`; this trait is the seam
//! the policy reads and writes through, so the only thing that differs between a
//! native and a browser build is which backend is plugged in. [`LruBackend`] is
//! the resident byte-bounded LRU used on every target (and the serving copy in
//! the browser); the IndexedDB-mirroring backend is feature-gated.

use nectar_primitives::ChunkAddress;
use vertex_store::BoundedLruStore;

use crate::chunk_store::CacheValue;

#[cfg(all(feature = "indexeddb", target_arch = "wasm32"))]
mod indexeddb;
#[cfg(all(feature = "indexeddb", target_arch = "wasm32"))]
pub use indexeddb::IndexedDbBackend;

/// Point byte-store operations the cache policy drives. Method names and
/// signatures mirror [`BoundedLruStore`] so the policy impl is backend-agnostic.
pub trait CacheBackend: Send + Sync {
    /// Insert or replace the value at `address`, evicting under pressure.
    fn insert(&self, address: ChunkAddress, value: CacheValue);

    /// Fetch a clone of the value at `address`, touching recency on a hit.
    fn get(&self, address: &ChunkAddress) -> Option<CacheValue>;

    /// Whether a value is resident for `address` (does not touch recency).
    fn contains(&self, address: &ChunkAddress) -> bool;

    /// Remove the value at `address`, freeing its budget.
    fn remove(&self, address: &ChunkAddress);

    /// The number of resident entries.
    fn len(&self) -> usize;

    /// Whether the backend holds no entries.
    fn is_empty(&self) -> bool;
}

/// The default backend: an in-memory byte-bounded LRU.
pub struct LruBackend(BoundedLruStore<ChunkAddress, CacheValue>);

impl LruBackend {
    /// Create a backend bounded to `max_bytes` of resident value bytes.
    #[must_use]
    pub fn with_budget(max_bytes: usize) -> Self {
        Self(BoundedLruStore::with_budget(max_bytes))
    }
}

impl CacheBackend for LruBackend {
    fn insert(&self, address: ChunkAddress, value: CacheValue) {
        self.0.insert(address, value);
    }

    fn get(&self, address: &ChunkAddress) -> Option<CacheValue> {
        self.0.get(address)
    }

    fn contains(&self, address: &ChunkAddress) -> bool {
        self.0.contains(address)
    }

    fn remove(&self, address: &ChunkAddress) {
        self.0.remove(address);
    }

    fn len(&self) -> usize {
        self.0.len()
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
