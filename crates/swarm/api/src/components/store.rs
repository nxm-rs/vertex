//! Local chunk storage.

use crate::SwarmResult;
use nectar_primitives::{AnyChunk, ChunkAddress};

/// Local chunk storage trait for Storer nodes.
///
/// Storer nodes store chunks they're responsible for. This is the local
/// persistence layer, separate from network operations.
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
