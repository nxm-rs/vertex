//! Core Swarm traits for network access.
//!
//! - [`SwarmReader`] - Read chunks from the swarm
//! - [`SwarmWriter`] - Write chunks to the swarm

use crate::SwarmResult;
use async_trait::async_trait;
use vertex_primitives::{AnyChunk, ChunkAddress};

/// Read chunks from the Swarm network.
///
/// Pure behavior trait - implementors decide their internal structure.
/// For topology/accounting access, use the concrete type's methods.
#[async_trait]
pub trait SwarmReader: Send + Sync {
    /// Get a chunk from the swarm by its address.
    async fn get(&self, address: &ChunkAddress) -> SwarmResult<AnyChunk>;
}

/// Write chunks to the Swarm network.
///
/// The associated `Storage` type represents the storage proof
/// (postage stamps on mainnet, `()` for development).
#[async_trait]
pub trait SwarmWriter: SwarmReader {
    /// Storage proof type (e.g., postage stamp).
    type Storage: Send + Sync + 'static;

    /// Put a chunk into the swarm with storage proof.
    async fn put(&self, chunk: AnyChunk, storage: &Self::Storage) -> SwarmResult<()>;
}
