//! Core Swarm traits for network access.

use crate::SwarmResult;
use vertex_primitives::{AnyChunk, ChunkAddress};

/// Unified client for reading and writing chunks to the Swarm network.
///
/// The `Storage` type is the storage proof (postage stamps on mainnet, `()` for dev).
#[async_trait::async_trait]
pub trait SwarmClient: Send + Sync {
    /// Storage proof type (e.g., postage stamp).
    type Storage: Send + Sync + 'static;

    /// Get a chunk from the swarm by its address.
    async fn get(&self, address: &ChunkAddress) -> SwarmResult<AnyChunk>;

    /// Put a chunk into the swarm with storage proof.
    async fn put(&self, chunk: AnyChunk, storage: &Self::Storage) -> SwarmResult<()>;
}
