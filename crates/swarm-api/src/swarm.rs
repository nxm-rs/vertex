//! Core Swarm traits for network access.
//!
//! - [`SwarmReader`] - Read-only access (requires [`LightTypes`])
//! - [`SwarmWriter`] - Read-write access (requires [`PublisherTypes`])

use crate::{LightTypes, PublisherTypes, SwarmResult};
use async_trait::async_trait;
use vertex_primitives::{AnyChunk, ChunkAddress};

/// Read-only access to the Swarm network.
///
/// Generic over `Types` which must implement [`LightTypes`] to provide
/// topology and accounting capabilities.
#[async_trait]
pub trait SwarmReader<Types: LightTypes>: Send + Sync {
    /// Get the topology for peer discovery and routing.
    fn topology(&self) -> &Types::Topology;

    /// Get the availability accounting for retrieval incentives.
    fn accounting(&self) -> &Types::Accounting;

    /// Get a chunk from the swarm by its address.
    async fn get(&self, address: &ChunkAddress) -> SwarmResult<AnyChunk>;
}

/// Read-write access to the Swarm network.
///
/// Generic over `Types` which must implement [`PublisherTypes`] to provide
/// storage proof capability (postage stamps on mainnet).
#[async_trait]
pub trait SwarmWriter<Types: PublisherTypes>: SwarmReader<Types> {
    /// Put a chunk into the swarm with storage proof.
    async fn put(&self, chunk: AnyChunk, storage: &Types::Storage) -> SwarmResult<()>;
}
