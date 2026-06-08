//! RPC provider traits for Swarm protocol.
//!
//! Data interfaces for RPC services, abstracting over concrete implementations.

use alloy_primitives::Signature;
use bytes::Bytes;
use nectar_primitives::{ChunkAddress, Nonce};
use vertex_swarm_primitives::{OverlayAddress, Stamp, StorageRadius};

use crate::SwarmResult;

/// Result of a successful chunk retrieval.
#[derive(Debug, Clone)]
pub struct ChunkRetrievalResult {
    /// The chunk data.
    pub data: Bytes,
    /// The postage stamp.
    pub stamp: Stamp,
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

/// Receipt for a chunk accepted by a storer via PushSync.
#[derive(Debug, Clone)]
pub struct PushReceipt {
    /// Overlay address of the storer that accepted this chunk.
    pub storer: OverlayAddress,
    /// The storer's signature over the receipt.
    pub signature: Signature,
    /// The nonce used by the storer in signing.
    pub nonce: Nonce,
    /// The storer's storage radius at the time of acceptance.
    pub storage_radius: StorageRadius,
}

/// Trait for sending chunks to the Swarm network via PushSync.
///
/// Client nodes use this to upload chunks. A chunk and its postage stamp travel
/// together, so both are passed to each method. Two modes are provided:
/// - `send_chunk_unchecked`: Trust the caller, no validation
/// - `send_chunk`: Validate stamp signature (but not batch validity)
#[async_trait::async_trait]
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait SwarmChunkSender: Send + Sync + 'static {
    /// Send a chunk and its stamp without any stamp validation.
    ///
    /// Trusts the caller has already validated the stamp. Use when:
    /// - Uploading freshly created chunks with known-good stamps
    /// - Internal operations where validation is redundant
    async fn send_chunk_unchecked(
        &self,
        chunk: nectar_primitives::AnyChunk,
        stamp: Stamp,
    ) -> SwarmResult<PushReceipt>;

    /// Send a chunk and its stamp with stamp signature validation.
    ///
    /// Validates the stamp signature matches the chunk address, but does NOT
    /// check batch validity on-chain. Batch validity is the storer's concern.
    ///
    /// Returns `SwarmError::InvalidSignature` if the stamp doesn't match the chunk.
    async fn send_chunk(
        &self,
        chunk: nectar_primitives::AnyChunk,
        stamp: Stamp,
    ) -> SwarmResult<PushReceipt>;
}
