//! Local chunk storage.

use crate::SwarmResult;
use nectar_primitives::ChunkAddress;
use vertex_swarm_primitives::StampedChunk;

/// Configuration for a local chunk store.
///
/// The client cache reads `cache_budget_bytes` to size its byte-bounded LRU and
/// `soc_cache_ttl` to decide how long a cached single-owner chunk stays
/// serveable. A persisting storer reserve will read its own sizing from the same
/// config surface.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmLocalStoreConfig {
    /// Resident memory budget for the cache, in bytes.
    fn cache_budget_bytes(&self) -> u64;

    /// How long a cached single-owner chunk stays serveable, in nanoseconds,
    /// measured against the stamp's signed timestamp. Content chunks ignore it.
    fn soc_cache_ttl(&self) -> u64;
}

/// Local chunk storage.
///
/// One abstraction for both the client cache (lossy, in-memory) and the storer
/// reserve (persisting); the difference is the implementation's backend and
/// eviction policy, not the trait. The stamp is held with the chunk so the value
/// type can never drop it: a [`StampedChunk`] is the currency everywhere.
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait SwarmLocalStore: Send + Sync {
    /// Insert a stamped chunk. Implementations evict under pressure (LRU for a
    /// cache, furthest-from-neighbourhood for a reserve); a client cache insert
    /// is effectively infallible (it makes room), a reserve may surface a
    /// capacity error.
    fn put(&self, chunk: StampedChunk) -> SwarmResult<()>;

    /// Fetch a stored stamped chunk, or `None` on a miss.
    fn get(&self, address: &ChunkAddress) -> SwarmResult<Option<StampedChunk>>;

    /// Check if a chunk exists locally.
    fn contains(&self, address: &ChunkAddress) -> bool;

    /// Remove a chunk from local storage.
    fn remove(&self, address: &ChunkAddress) -> SwarmResult<()>;
}
