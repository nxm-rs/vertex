//! Core Swarm traits for network access.

use crate::SwarmResult;
use nectar_primitives::{AnyChunk, ChunkAddress};
use vertex_swarm_primitives::{OverlayAddress, StampedChunk, StorageRadius};

/// Client node capability - chunk retrieval and upload.
#[async_trait::async_trait]
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait SwarmClient: Send + Sync {
    /// Get a chunk from the swarm by its address.
    ///
    /// A download returns the chunk itself; the stamp that authorized its
    /// storage is dropped on the way out.
    async fn get(&self, address: &ChunkAddress) -> SwarmResult<AnyChunk>;

    /// Put a chunk and its stamp into the swarm.
    async fn put(&self, chunk: StampedChunk) -> SwarmResult<()>;
}

/// Storer node capability - storage responsibility and sync.
#[async_trait::async_trait]
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait SwarmStorer: Send + Sync {
    /// Check if a chunk falls within our area of responsibility.
    fn is_responsible_for(&self, address: &ChunkAddress) -> bool;

    /// Current storage radius (this node's reserve / responsibility radius).
    fn storage_radius(&self) -> StorageRadius;

    /// Sync chunks with a neighbor peer. Returns chunks received.
    async fn sync(&self, peer: &OverlayAddress) -> SwarmResult<usize>;
}
