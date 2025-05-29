//! Network-related traits
//!
//! This module defines the traits for network communication in the Swarm network.

use alloc::{boxed::Box, string::String, vec::Vec};
use async_trait::async_trait;
use core::fmt::Debug;
use vertex_primitives::{ChunkAddress, PeerId, Result};

use crate::{access::Credential, chunk::Chunk};

/// Network status information
#[derive(Debug, Clone)]
pub struct NetworkStatus {
    /// Number of connected peers
    pub connected_peers: usize,
    /// Neighborhood depth (radius of responsibility)
    pub neighborhood_depth: u8,
    /// Estimated network size
    pub estimated_network_size: usize,
    /// Whether the node is connected to the network
    pub is_connected: bool,
    /// Network bandwidth usage statistics
    pub bandwidth_stats: BandwidthStats,
}

/// Bandwidth usage statistics
#[derive(Debug, Clone, Default)]
pub struct BandwidthStats {
    /// Total bytes sent
    pub bytes_sent: u64,
    /// Total bytes received
    pub bytes_received: u64,
    /// Current upload rate in bytes per second
    pub upload_rate_bps: u64,
    /// Current download rate in bytes per second
    pub download_rate_bps: u64,
}

/// Core trait for network operations
#[async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait NetworkClient: Send + Sync + 'static {
    /// The credential type used by this client
    type Credential: Credential;

    /// Retrieve a chunk from the network
    async fn retrieve(
        &self,
        address: &ChunkAddress,
        credential: Option<&Self::Credential>,
    ) -> Result<Box<dyn Chunk>>;

    /// Store a chunk in the network
    async fn store(
        &self,
        chunk: Box<dyn Chunk>,
        credential: &Self::Credential,
    ) -> Result<()>;

    /// Connect to the Swarm network
    async fn connect(&self) -> Result<()>;

    /// Disconnect from the Swarm network
    async fn disconnect(&self) -> Result<()>;

    /// Get current network status
    fn status(&self) -> NetworkStatus;

    /// List connected peers
    fn connected_peers(&self) -> Vec<PeerId>;

    /// Find closest peers to an address
    async fn find_closest(
        &self,
        address: &ChunkAddress,
        limit: usize,
    ) -> Result<Vec<PeerId>>;
}

/// Factory for creating network client implementations
#[auto_impl::auto_impl(&, Arc)]
pub trait NetworkClientFactory: Send + Sync + 'static {
    /// The type of network client this factory creates
    type Client: NetworkClient;

    /// Create a new network client with the given configuration
    fn create_client(&self, config: &NetworkConfig) -> Result<Self::Client>;
}

/// Network configuration
#[derive(Debug, Clone)]
pub struct NetworkConfig {
    /// Local listening address
    pub listen_addr: String,
    /// Bootnodes to connect to
    pub bootnodes: Vec<String>,
    /// Network ID
    pub network_id: u64,
    /// Maximum number of peers
    pub max_peers: usize,
    /// Whether to enable peer discovery
    pub enable_discovery: bool,
    /// Custom node identity
    pub node_identity: Option<String>,
}

/// Protocol message handler
#[async_trait]
pub trait MessageHandler: Send + Sync + 'static {
    /// The protocol ID this handler is responsible for
    fn protocol_id(&self) -> &str;

    /// Handle an incoming message from a peer
    async fn handle_message(&self, peer: &PeerId, message: &[u8]) -> Result<Option<Vec<u8>>>;

    /// Called when a peer connects
    async fn peer_connected(&self, peer: &PeerId) -> Result<()>;

    /// Called when a peer disconnects
    async fn peer_disconnected(&self, peer: &PeerId) -> Result<()>;
}

/// Peer discovery service
#[async_trait]
pub trait Discovery: Send + Sync + 'static {
    /// Start the discovery service
    async fn start(&self) -> Result<()>;

    /// Stop the discovery service
    async fn stop(&self) -> Result<()>;

    /// Add a node to the discovery service
    async fn add_node(&self, peer: PeerId, addresses: Vec<String>) -> Result<()>;

    /// Find nodes closest to the given address
    async fn find_nodes(&self, target: &ChunkAddress, limit: usize) -> Result<Vec<PeerId>>;

    /// Get all known nodes
    fn get_known_nodes(&self) -> Vec<(PeerId, Vec<String>)>;
}
