//! RPC provider traits for Swarm protocol.
//!
//! Data interfaces for RPC services, abstracting over concrete implementations.

use bytes::Bytes;
use nectar_primitives::ChunkAddress;
use vertex_swarm_primitives::OverlayAddress;

use crate::SwarmResult;

/// Result of a successful chunk retrieval.
#[derive(Debug, Clone)]
pub struct ChunkRetrievalResult {
    /// The chunk data.
    pub data: Bytes,
    /// The postage stamp.
    pub stamp: Bytes,
    /// Overlay address of the peer that served this chunk.
    pub served_by: OverlayAddress,
}

/// Provider trait for chunk retrieval operations.
#[async_trait::async_trait]
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait SwarmChunkProvider: Send + Sync + 'static {
    /// Retrieve a chunk by its address from the Swarm network.
    async fn retrieve_chunk(&self, address: &ChunkAddress) -> SwarmResult<ChunkRetrievalResult>;

    /// Check if a chunk exists locally.
    ///
    /// Returns false for light nodes that don't have local storage.
    fn has_chunk(&self, address: &ChunkAddress) -> bool;
}

/// Result of a successful chunk send via PushSync.
#[derive(Debug, Clone)]
pub struct ChunkSendReceipt {
    /// Overlay address of the storer that accepted this chunk.
    pub storer: OverlayAddress,
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
    ) -> SwarmResult<ChunkSendReceipt>;

    /// Send a chunk with stamp signature validation.
    ///
    /// Validates the stamp signature matches the chunk address, but does NOT
    /// check batch validity on-chain. Batch validity is the storer's concern.
    ///
    /// Returns `SwarmError::InvalidSignature` if the stamp doesn't match the chunk.
    async fn send_chunk(&self, chunk: nectar_primitives::AnyChunk)
        -> SwarmResult<ChunkSendReceipt>;
}
