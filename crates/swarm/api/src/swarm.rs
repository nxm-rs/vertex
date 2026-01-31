//! Core Swarm traits for network access.

use crate::SwarmResult;
use nectar_primitives::{AnyChunk, ChunkAddress};
use vertex_swarm_primitives::OverlayAddress;

/// Client node capability - chunk retrieval and upload.
#[async_trait::async_trait]
pub trait SwarmClient: Send + Sync {
    /// Storage proof type (e.g., postage stamp).
    type Storage: Send + Sync + 'static;

    /// Get a chunk from the swarm by its address.
    async fn get(&self, address: &ChunkAddress) -> SwarmResult<AnyChunk>;

    /// Put a chunk into the swarm with storage proof.
    async fn put(&self, chunk: AnyChunk, storage: &Self::Storage) -> SwarmResult<()>;
}

/// Storer node capability - storage responsibility and sync.
#[async_trait::async_trait]
pub trait SwarmStorer: Send + Sync {
    /// Check if a chunk falls within our area of responsibility.
    fn is_responsible_for(&self, address: &ChunkAddress) -> bool;

    /// Current storage radius (neighborhood depth threshold).
    fn storage_radius(&self) -> u8;

    /// Sync chunks with a neighbor peer. Returns chunks received.
    async fn sync(&self, peer: &OverlayAddress) -> SwarmResult<usize>;
}
