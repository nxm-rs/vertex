//! Node lifecycle trait.
//!
//! Defines the interface for starting, stopping, and managing nodes.

use async_trait::async_trait;
use vertex_swarm_api::SwarmResult;

/// Node lifecycle management.
///
/// This trait defines how to start and stop a node, regardless of
/// whether it's a light node or full node.
#[async_trait]
pub trait Node: Send + Sync {
    /// Start the node.
    ///
    /// This should:
    /// - Connect to the network
    /// - Start any background tasks
    /// - Begin syncing (for full nodes)
    async fn start(&self) -> SwarmResult<()>;

    /// Stop the node gracefully.
    ///
    /// This should:
    /// - Disconnect from peers
    /// - Stop background tasks
    /// - Flush any pending state
    async fn stop(&self) -> SwarmResult<()>;

    /// Check if the node is currently running.
    fn is_running(&self) -> bool;
}

/// Node health information.
#[derive(Debug, Clone, Default)]
pub struct NodeHealth {
    /// Whether the node is running.
    pub running: bool,
    /// Number of connected peers.
    pub connected_peers: usize,
    /// Current neighborhood depth.
    pub depth: u8,
}

/// Storage statistics for full nodes.
#[derive(Debug, Clone, Default)]
pub struct StorageStats {
    /// Number of chunks stored locally.
    pub chunks_stored: u64,
    /// Bytes used by chunk storage.
    pub bytes_used: u64,
    /// Bytes available for storage.
    pub bytes_available: u64,
}

/// Sync status for full nodes.
#[derive(Debug, Clone, Default)]
pub struct SyncStatus {
    /// Whether currently syncing.
    pub is_syncing: bool,
    /// Total chunks synced.
    pub chunks_synced: u64,
    /// Number of peers synced with.
    pub peers_synced_with: usize,
}

/// Extended interface for full nodes.
#[async_trait]
pub trait FullNode: Node {
    /// Get storage statistics.
    fn storage_stats(&self) -> StorageStats;

    /// Get sync status.
    fn sync_status(&self) -> SyncStatus;
}
