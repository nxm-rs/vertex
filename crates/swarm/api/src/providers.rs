//! RPC provider traits for Swarm protocol.
//!
//! Data interfaces for RPC services, abstracting over concrete implementations.

use bytes::Bytes;

/// Result of a successful chunk retrieval.
#[derive(Debug, Clone)]
pub struct ChunkRetrievalResult {
    /// The chunk data.
    pub data: Bytes,
    /// The postage stamp.
    pub stamp: Bytes,
    /// Overlay address of the peer that served this chunk (hex encoded).
    pub served_by: String,
}

/// Error from chunk retrieval operations.
#[derive(Debug, thiserror::Error)]
pub enum ChunkRetrievalError {
    /// Chunk not found in the network.
    #[error("Chunk not found: {0}")]
    NotFound(String),
    /// Network error.
    #[error("Network error: {0}")]
    Network(String),
    /// Invalid address format.
    #[error("Invalid address: {0}")]
    InvalidAddress(String),
    /// Internal error.
    #[error("Internal error: {0}")]
    Internal(String),
}

/// Provider trait for chunk retrieval operations.
#[async_trait::async_trait]
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait SwarmChunkProvider: Send + Sync + 'static {
    /// Retrieve a chunk by its address from the Swarm network.
    ///
    /// The address should be a 64-character hex string.
    async fn retrieve_chunk(&self, address: &str) -> Result<ChunkRetrievalResult, ChunkRetrievalError>;

    /// Check if a chunk exists locally.
    ///
    /// Returns false for light nodes that don't have local storage.
    fn has_chunk(&self, address: &str) -> bool;
}

/// Provider trait for topology and network status information.
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait SwarmTopologyProvider: Send + Sync + 'static {
    /// Get the node's overlay address as a hex-encoded string.
    fn overlay_address(&self) -> String;

    /// Get the current Kademlia depth.
    ///
    /// Depth indicates how "deep" into the address space we're responsible for.
    fn depth(&self) -> u8;

    /// Get the count of currently connected peers.
    fn connected_peers_count(&self) -> usize;

    /// Get the count of known (discovered but not necessarily connected) peers.
    fn known_peers_count(&self) -> usize;

    /// Get the count of pending connection attempts.
    fn pending_connections_count(&self) -> usize;

    /// Get bin sizes for each proximity order (0-31).
    ///
    /// Returns a vector of `(connected, known)` tuples, one per bin.
    fn bin_sizes(&self) -> Vec<(usize, usize)>;

    /// Get connected peer overlay addresses in a specific bin.
    ///
    /// Returns hex-encoded overlay addresses.
    fn connected_peers_in_bin(&self, po: u8) -> Vec<String>;
}

/// Result of a successful chunk send via PushSync.
#[derive(Debug, Clone)]
pub struct ChunkSendReceipt {
    /// Overlay address of the storer that accepted this chunk (hex encoded).
    pub storer: String,
}

/// Error from chunk send operations.
#[derive(Debug, thiserror::Error)]
pub enum ChunkSendError {
    /// No storer found for this chunk.
    #[error("No storer found in proximity: {0}")]
    NoStorer(String),
    /// Invalid stamp signature.
    #[error("Invalid stamp signature: {0}")]
    InvalidSignature(String),
    /// Network error.
    #[error("Network error: {0}")]
    Network(String),
    /// Internal error.
    #[error("Internal error: {0}")]
    Internal(String),
}

/// Trait for sending chunks to the Swarm network via PushSync.
///
/// Client nodes use this to upload chunks. Two modes are provided:
/// - `send_chunk_unchecked`: Trust the caller, no validation
/// - `send_chunk`: Validate stamp signature (but not batch validity)
#[async_trait::async_trait]
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait SwarmChunkSender: Send + Sync + 'static {
    /// Send a chunk without any stamp validation.
    ///
    /// Trusts the caller has already validated the stamp. Use when:
    /// - Uploading freshly created chunks with known-good stamps
    /// - Internal operations where validation is redundant
    async fn send_chunk_unchecked(
        &self,
        chunk: nectar_primitives::AnyChunk,
    ) -> Result<ChunkSendReceipt, ChunkSendError>;

    /// Send a chunk with stamp signature validation.
    ///
    /// Validates the stamp signature matches the chunk address, but does NOT
    /// check batch validity on-chain. Batch validity is the storer's concern.
    ///
    /// Returns `InvalidSignature` if the stamp doesn't match the chunk.
    async fn send_chunk(
        &self,
        chunk: nectar_primitives::AnyChunk,
    ) -> Result<ChunkSendReceipt, ChunkSendError>;
}

// Future providers can be added here:
//
// /// Provider trait for accounting/incentive information.
// pub trait SwarmAccountingProvider: Send + Sync + 'static {
//     fn peer_balance(&self, peer: &OverlayAddress) -> i64;
//     fn total_sent(&self) -> u64;
//     fn total_received(&self) -> u64;
// }
//
// /// Provider trait for storage information.
// pub trait SwarmStorageProvider: Send + Sync + 'static {
//     fn capacity(&self) -> u64;
//     fn used(&self) -> u64;
//     fn chunk_count(&self) -> u64;
// }
