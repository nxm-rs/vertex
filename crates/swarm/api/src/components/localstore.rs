//! Local chunk storage.

use crate::SwarmResult;
use nectar_primitives::{AnyChunk, ChunkAddress};

/// Configuration for local store.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmLocalStoreConfig {
    /// Cache capacity in number of chunks.
    fn cache_chunks(&self) -> u64;
}

/// Local chunk storage for Storer nodes.
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait SwarmLocalStore: Send + Sync {
    /// Store a chunk locally.
    fn store(&self, chunk: &AnyChunk) -> SwarmResult<()>;

    /// Retrieve a chunk from local storage.
    fn retrieve(&self, address: &ChunkAddress) -> SwarmResult<Option<AnyChunk>>;

    /// Check if a chunk exists locally.
    fn has(&self, address: &ChunkAddress) -> bool;

    /// Remove a chunk from local storage.
    fn remove(&self, address: &ChunkAddress) -> SwarmResult<()>;
}
