//! Node type traits
//!
//! This module defines the traits for different node types in the Swarm network.

use alloc::{boxed::Box, string::String, vec::Vec};
use async_trait::async_trait;
use core::fmt::Debug;
use vertex_primitives::{ChunkAddress, Result};

use crate::{
    access::Credential,
    bandwidth::{BandwidthConfig, BandwidthStatus},
    chunk::Chunk,
    network::{NetworkConfig, NetworkStatus},
    storage::{ChunkStore, StorageConfig, StorageStats},
};

/// Node mode (light, full, or incentivized)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeMode {
    /// Light client only
    Light,
    /// Full node with storage
    Full,
    /// Full node participating in storage incentives
    Incentivized,
}

/// Node configuration
#[derive(Debug, Clone)]
pub struct NodeConfig {
    /// Node operating mode
    pub mode: NodeMode,
    /// Network configuration
    pub network: NetworkConfig,
    /// Storage configuration
    pub storage: StorageConfig,
    /// Bandwidth configuration
    pub bandwidth: BandwidthConfig,
    /// Network ID
    pub network_id: u64,
    /// API endpoint configuration
    pub api_endpoint: String,
    /// Metrics endpoint configuration
    pub metrics_endpoint: Option<String>,
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
}

/// Extended trait for full node functionality
#[async_trait]
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

/// Status of storage incentives
#[derive(Debug, Clone)]
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

/// Factory for creating node instances
#[auto_impl::auto_impl(&, Arc)]
pub trait NodeFactory: Send + Sync + 'static {
    /// Create a new node with the given configuration
    fn create_node(&self, config: NodeConfig) -> Result<Box<dyn SwarmBaseNode>>;

    /// Create a light client node
    fn create_light_node(&self, config: NodeConfig) -> Result<Box<dyn SwarmBaseNode>>;

    /// Create a full node
    fn create_full_node(&self, config: NodeConfig) -> Result<Box<dyn SwarmFullNode>>;

    /// Create an incentivized node
    fn create_incentivized_node(&self, config: NodeConfig) -> Result<Box<dyn SwarmIncentivizedNode>>;
}
