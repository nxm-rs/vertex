//! Chunk synchronization between peers using overlay addresses.

use nectar_primitives::AnyChunk;
use vertex_swarm_primitives::OverlayAddress;

use crate::SwarmResult;

/// Chunk synchronization trait between peers.
///
/// Full nodes sync chunks with neighbors to ensure data availability.
#[async_trait::async_trait]
pub trait SwarmChunkSync: Send + Sync {
    /// Sync chunks with a peer.
    ///
    /// Returns statistics about what was synced.
    /// The peer is identified by their overlay address.
    async fn sync_with(&self, peer: &OverlayAddress) -> SwarmResult<SyncResult>;

    /// Offer a chunk to the network (push sync).
    ///
    /// The chunk will be forwarded to peers responsible for storing it.
    async fn offer(&self, chunk: &AnyChunk) -> SwarmResult<()>;
}

/// Result of a sync operation.
#[derive(Debug, Clone, Default)]
pub struct SyncResult {
    /// Chunks received from peer.
    pub received: u64,
    /// Chunks sent to peer.
    pub sent: u64,
}
