//! Node API definitions for the Vertex Swarm
//!
//! This crate defines the interfaces for the Vertex Swarm node.

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]

extern crate alloc;

use async_trait::async_trait;
use vertex_primitives::{ChunkAddress, Error, Result};
use vertex_swarm_api::{
    access::{AccessController, Credential},
    bandwidth::BandwidthController,
    chunk::Chunk,
    network::NetworkClient,
    storage::ChunkStore,
};
use vertex_swarmspec::SwarmSpec;

/// Traits, validation methods, and helper types used for node components.
pub mod components;
pub use components::*;

/// Traits and helper types for node types.
pub mod types;
pub use types::*;

/// Traits for node events.
pub mod events;
pub use events::*;

/// Traits for node configuration.
pub mod config;
pub use config::*;

/// Re-export key traits from swarm-api for convenience
pub use vertex_swarm_api::{
    chunk::ChunkType,
    node::{NodeMode, SwarmBaseNode, SwarmFullNode, SwarmIncentivizedNode},
    types::SwarmEvent,
};

// Re-export common primitives
pub use vertex_primitives::{ChunkAddress, Error, Result};

/// Core node trait that encompasses all node functionality
#[async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait VertexNode: Send + Sync + 'static {
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

    /// Store a chunk in the network
    async fn store(
        &self,
        chunk: Box<dyn Chunk>,
        credential: &Self::Credential,
    ) -> Result<()>;

    /// Retrieve a chunk from the network
    async fn retrieve(
        &self,
        address: &ChunkAddress,
        credential: Option<&Self::Credential>,
    ) -> Result<Box<dyn Chunk>>;

    /// Get the node's operating mode
    fn mode(&self) -> NodeMode;

    /// Get the node's store
    fn store(&self) -> &Self::Store;

    /// Get the node's network client
    fn network(&self) -> &Self::Network;

    /// Get the node's access controller
    fn access_controller(&self) -> &Self::AccessController;

    /// Get the node's bandwidth controller
    fn bandwidth_controller(&self) -> &Self::BandwidthController;

    /// Get the node's configuration
    fn config(&self) -> &NodeConfig;

    /// Get the Swarm specification
    fn spec(&self) -> &SwarmSpec;

    /// Start the node
    async fn start(&self) -> Result<()>;

    /// Stop the node
    async fn stop(&self) -> Result<()>;

    /// Subscribe to node events
    fn subscribe(&self) -> Box<dyn Iterator<Item = SwarmEvent> + Send + '_>;
}

/// Node configuration
#[derive(Debug, Clone)]
pub struct NodeConfig {
    /// Node operating mode
    pub mode: NodeMode,
    /// Data directory
    pub data_dir: alloc::string::String,
    /// Network ID
    pub network_id: u64,
    /// Network options
    pub network: NetworkOptions,
    /// Storage options
    pub storage: StorageOptions,
    /// API options
    pub api: ApiOptions,
    /// Metrics options
    pub metrics: Option<MetricsOptions>,
}

/// Network configuration options
#[derive(Debug, Clone)]
pub struct NetworkOptions {
    /// Listen address
    pub listen_addr: alloc::string::String,
    /// Bootnodes
    pub bootnodes: alloc::vec::Vec<alloc::string::String>,
    /// Maximum number of peers
    pub max_peers: usize,
    /// Enable peer discovery
    pub enable_discovery: bool,
}

/// Storage configuration options
#[derive(Debug, Clone)]
pub struct StorageOptions {
    /// Maximum storage space in bytes
    pub max_space: u64,
    /// Storage directory
    pub storage_dir: alloc::string::String,
}

/// API configuration options
#[derive(Debug, Clone)]
pub struct ApiOptions {
    /// Enable HTTP API
    pub enable_http: bool,
    /// HTTP listen address
    pub http_addr: alloc::string::String,
    /// Enable gRPC API
    pub enable_grpc: bool,
    /// gRPC listen address
    pub grpc_addr: alloc::string::String,
}

/// Metrics configuration options
#[derive(Debug, Clone)]
pub struct MetricsOptions {
    /// Enable Prometheus metrics
    pub enable_prometheus: bool,
    /// Prometheus listen address
    pub prometheus_addr: alloc::string::String,
}
