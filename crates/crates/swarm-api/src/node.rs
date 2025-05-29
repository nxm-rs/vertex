//! Node-related traits

use crate::{Chunk, Credential, NetworkStatus, Result, StorageStats};
use vertex_primitives::{ChunkAddress, NodeMode};

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::String, vec::Vec};

/// Status of storage incentives
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct IncentiveStatus {
    /// Whether this node is registered
    pub is_registered: bool,
    /// Total rewards earned
    pub total_rewards: u64,
    /// Unclaimed rewards amount
    pub unclaimed_rewards: u64,
    /// Last participation timestamp
    pub last_participation: u64,
    /// Staking status
    pub staking_status: Option<StakingStatus>,
}

/// Status of staking participation
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct StakingStatus {
    /// Amount staked
    pub stake_amount: u64,
    /// Staking tier
    pub tier: u8,
    /// Whether stake is locked
    pub is_locked: bool,
    /// Unlock timestamp if locked
    pub unlock_time: Option<u64>,
}

/// Node configuration
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NodeConfig {
    /// Node operating mode
    pub mode: NodeMode,
    /// Data directory
    pub data_dir: String,
    /// API endpoint configuration
    pub api_endpoint: Option<String>,
    /// Metrics endpoint configuration
    pub metrics_endpoint: Option<String>,
    /// Whether to enable debugging features
    pub debug: bool,
    /// Maximum log level
    pub log_level: String,
    /// Whether to show verbose output
    pub verbose: bool,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            mode: NodeMode::Light,
            data_dir: "./data".into(),
            api_endpoint: Some("127.0.0.1:1635".into()),
            metrics_endpoint: Some("127.0.0.1:1636".into()),
            debug: false,
            log_level: "info".into(),
            verbose: false,
        }
    }
}

/// Core trait for base node functionality (common to all modes)
#[async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmBaseNode: Send + Sync + 'static {
    /// The credential type used by this node
    type Credential: Credential;

    /// Store a chunk in the Swarm network
    async fn store(
        &self,
        chunk: Box<dyn Chunk>,
        credential: &Self::Credential,
    ) -> Result<()>;

    /// Retrieve a chunk from the Swarm network
    async fn retrieve(
        &self,
        address: &ChunkAddress,
        credential: Option<&Self::Credential>,
    ) -> Result<Box<dyn Chunk>>;

    /// Get the node's operating mode
    fn mode(&self) -> NodeMode;

    /// Get current network status
    fn network_status(&self) -> NetworkStatus;

    /// Connect to the Swarm network
    async fn connect(&self) -> Result<()>;

    /// Disconnect from the Swarm network
    async fn disconnect(&self) -> Result<()>;

    /// Get node status information
    fn status(&self) -> NodeStatus;
}

/// Node status information
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NodeStatus {
    /// Node operating mode
    pub mode: NodeMode,
    /// Whether the node is connected to the network
    pub connected: bool,
    /// Number of connected peers
    pub connected_peers: usize,
    /// Current network depth
    pub neighborhood_depth: u8,
    /// Uptime in seconds
    pub uptime: u64,
    /// Storage statistics
    pub storage_stats: Option<StorageStats>,
    /// Incentive status
    pub incentive_status: Option<IncentiveStatus>,
}

/// Extended trait for full node functionality
#[async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmFullNode: SwarmBaseNode {
    /// Check if this node is responsible for a given chunk address
    fn is_responsible_for(&self, address: &ChunkAddress) -> bool;

    /// Get storage statistics for this node
    fn storage_stats(&self) -> Result<StorageStats>;

    /// Synchronize chunks this node is responsible for
    async fn sync_responsible_chunks(&self) -> Result<()>;

    /// Get the current neighborhood depth
    fn neighborhood_depth(&self) -> u8;

    /// Recalculate neighborhood depth based on current network conditions
    async fn recalculate_depth(&self) -> Result<u8>;
}

/// Extended trait for nodes participating in storage incentives
#[async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmIncentivizedNode: SwarmFullNode {
    /// Register this node as a storage provider
    async fn register(&self) -> Result<()>;

    /// Participate in redistribution lottery
    async fn participate_in_redistribution(&self) -> Result<()>;

    /// Claim earned rewards
    async fn claim_rewards(&self) -> Result<u64>;

    /// Get current incentivization status
    fn incentive_status(&self) -> Result<IncentiveStatus>;
}
