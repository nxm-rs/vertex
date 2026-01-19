//! Local chunk storage.

use vertex_primitives::{AnyChunk, ChunkAddress, Result};

/// Local chunk storage for full nodes.
///
/// Full nodes store chunks they're responsible for. This is the local
/// persistence layer, separate from network operations.
pub trait LocalStore: Send + Sync {
    /// Store a chunk locally.
    fn store(&self, chunk: &AnyChunk) -> Result<()>;

    /// Retrieve a chunk from local storage.
    fn retrieve(&self, address: &ChunkAddress) -> Result<Option<AnyChunk>>;

    /// Check if a chunk exists locally.
    fn has(&self, address: &ChunkAddress) -> bool;

    /// Remove a chunk from local storage.
    fn remove(&self, address: &ChunkAddress) -> Result<()>;
}
