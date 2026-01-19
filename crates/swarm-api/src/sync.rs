//! Chunk synchronization.
//!
//! This module defines the [`ChunkSync`] trait for syncing chunks between peers.
//! All operations use [`OverlayAddress`] (not libp2p `PeerId`) since sync is
//! based on Swarm overlay addresses.

use async_trait::async_trait;
use vertex_primitives::{AnyChunk, OverlayAddress, Result};

/// Chunk synchronization between peers.
///
/// Full nodes sync chunks with neighbors to ensure data availability.
/// This is how chunks get distributed across the network.
///
/// # Overlay Addresses
///
/// All sync operations use [`OverlayAddress`] for peer identification.
/// The overlay address determines which peers should store which chunks
/// based on Kademlia proximity.
#[async_trait]
pub trait ChunkSync: Send + Sync {
    /// Sync chunks with a peer.
    ///
    /// Returns statistics about what was synced.
    /// The peer is identified by their overlay address.
    async fn sync_with(&self, peer: &OverlayAddress) -> Result<SyncResult>;

    /// Offer a chunk to the network (push sync).
    ///
    /// The chunk will be forwarded to peers responsible for storing it.
    async fn offer(&self, chunk: &AnyChunk) -> Result<()>;
}

/// Result of a sync operation.
#[derive(Debug, Clone, Default)]
pub struct SyncResult {
    /// Chunks received from peer.
    pub received: u64,
    /// Chunks sent to peer.
    pub sent: u64,
}
