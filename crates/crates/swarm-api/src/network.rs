//! Network-related traits

use crate::{Chunk, Credential, Result};
use vertex_primitives::{ChunkAddress, NetworkStatus, PeerId};

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::String, vec::Vec};
use core::fmt::Debug;

/// Network configuration
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
    /// Bootstrap node mode (will not participate in storage)
    pub bootstrap_node: bool,
    /// Maximum number of connection attempts
    pub max_connection_attempts: usize,
    /// Connection timeout in seconds
    pub connection_timeout: u64,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            listen_addr: "/ip4/0.0.0.0/tcp/1634".into(),
            bootnodes: Vec::new(),
            network_id: 1, // Default to mainnet
            max_peers: 50,
            enable_discovery: true,
            node_identity: None,
            bootstrap_node: false,
            max_connection_attempts: 3,
            connection_timeout: 30,
        }
    }
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
    /// Create a new network client with the given configuration
    fn create_client(&self, config: &NetworkConfig) -> Result<Box<dyn NetworkClient<Credential = Self::Credential>>>;

    /// The credential type used by created clients
    type Credential: Credential;
}
