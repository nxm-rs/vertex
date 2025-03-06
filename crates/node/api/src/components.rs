//! Component traits for the Vertex Swarm node
//!
//! This module defines the interfaces for the various components that make up a node.

use alloc::boxed::Box;
use async_trait::async_trait;
use vertex_primitives::{ChunkAddress, Result};
use vertex_swarm_api::{
    access::{AccessController, Credential},
    bandwidth::BandwidthController,
    chunk::Chunk,
    network::NetworkClient,
    storage::ChunkStore,
};

use crate::{config::NodeConfigProvider, NodeConfig};

/// Core node components trait
#[auto_impl::auto_impl(&, Arc)]
pub trait NodeComponents: Send + Sync + 'static {
    /// The type of credential used by this node
    type Credential: Credential;

    /// The type of storage used by this node
    type Store: ChunkStore;

    /// The type of network client used by this node
    type Network: NetworkClient;

    /// The type of access controller used by this node
    type AccessController: AccessController;

    /// The type of bandwidth controller used by this node
    type BandwidthController: BandwidthController;

    /// Returns the node's store
    fn store(&self) -> &Self::Store;

    /// Returns the node's network client
    fn network(&self) -> &Self::Network;

    /// Returns the node's access controller
    fn access_controller(&self) -> &Self::AccessController;

    /// Returns the node's bandwidth controller
    fn bandwidth_controller(&self) -> &Self::BandwidthController;

    /// Returns the node's configuration
    fn config(&self) -> &NodeConfig;
}

/// A type that builds the components for a node
#[async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait NodeComponentsBuilder: Send + Sync + 'static {
    /// The components this builder creates
    type Components: NodeComponents;

    /// Build the components for a node
    async fn build_components(&self, config: &dyn NodeConfigProvider) -> Result<Self::Components>;
}

/// A trait for the chunk operations component
#[async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait ChunkOperations: Send + Sync + 'static {
    /// The credential type used by this component
    type Credential: Credential;

    /// Store a chunk
    async fn store(
        &self,
        chunk: Box<dyn Chunk>,
        credential: &Self::Credential,
    ) -> Result<()>;

    /// Retrieve a chunk
    async fn retrieve(
        &self,
        address: &ChunkAddress,
        credential: Option<&Self::Credential>,
    ) -> Result<Box<dyn Chunk>>;

    /// Check if a chunk exists
    async fn contains(&self, address: &ChunkAddress) -> Result<bool>;

    /// Delete a chunk
    async fn delete(&self, address: &ChunkAddress) -> Result<()>;
}

/// A trait for the retrieval operation component
#[async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait RetrievalOperations: Send + Sync + 'static {
    /// The credential type used by this component
    type Credential: Credential;

    /// Retrieve a chunk
    async fn retrieve(
        &self,
        address: &ChunkAddress,
        credential: Option<&Self::Credential>,
    ) -> Result<Box<dyn Chunk>>;

    /// Retrieve a chunk from a specific peer
    async fn retrieve_from_peer(
        &self,
        address: &ChunkAddress,
        peer: &vertex_primitives::PeerId,
        credential: Option<&Self::Credential>,
    ) -> Result<Box<dyn Chunk>>;
}

/// A trait for the storage operation component
#[async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait StorageOperations: Send + Sync + 'static {
    /// The credential type used by this component
    type Credential: Credential;

    /// Store a chunk
    async fn store(
        &self,
        chunk: Box<dyn Chunk>,
        credential: &Self::Credential,
    ) -> Result<()>;

    /// Check if a chunk exists
    fn contains(&self, address: &ChunkAddress) -> Result<bool>;

    /// Delete a chunk
    fn delete(&self, address: &ChunkAddress) -> Result<()>;

    /// Get storage stats
    fn stats(&self) -> Result<vertex_swarm_api::storage::StorageStats>;
}

/// A trait for the sync operation component
#[async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait SyncOperations: Send + Sync + 'static {
    /// Sync chunks with a specific peer
    async fn sync_with_peer(
        &self,
        peer: &vertex_primitives::PeerId,
        max_chunks: usize,
    ) -> Result<usize>;

    /// Sync chunks for the entire neighborhood
    async fn sync_neighborhood(&self, max_chunks: usize) -> Result<usize>;

    /// Get sync status
    fn sync_status(&self) -> Result<SyncStatus>;
}

/// Sync status information
#[derive(Debug, Clone)]
pub struct SyncStatus {
    /// Number of chunks synced
    pub chunks_synced: usize,
    /// Total chunks to sync
    pub total_chunks: usize,
    /// Progress percentage
    pub progress_percent: f32,
    /// Last sync time
    pub last_sync_time: u64,
    /// Whether sync is in progress
    pub in_progress: bool,
}
