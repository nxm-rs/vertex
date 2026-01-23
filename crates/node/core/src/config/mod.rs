//! Node configuration handling.
//!
//! Configuration is split into submodules:
//! - `network` - P2P networking settings
//! - `availability` - Availability incentive settings
//! - `storage` - Local storage settings
//! - `api` - HTTP/metrics API settings
//! - `identity` - Identity and nonce settings

mod api;
mod availability;
mod identity;
mod network;
mod storage;

pub use api::ApiConfig;
pub use availability::AvailabilityConfig;
pub use identity::{generate_random_nonce, IdentityConfig};
pub use network::NetworkConfig;
pub use storage::StorageConfig;

use crate::{
    cli::{
        ApiArgs, AvailabilityArgs, AvailabilityMode, NetworkArgs, StorageArgs, StorageIncentiveArgs,
    },
    constants::*,
};
use eyre::Result;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    net::{IpAddr, SocketAddr},
    path::Path,
    str::FromStr,
};
use vertex_swarmspec::Hive;

/// Node type determines what capabilities and protocols the node runs.
///
/// Each type builds on the capabilities of the previous:
/// - Bootnode: Only topology (Hive/Kademlia)
/// - Light: + Bandwidth accounting + Retrieval
/// - Publisher: + Upload/Postage
/// - Full: + Pullsync + Local storage
/// - Staker: + Redistribution game
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeType {
    /// Bootnode - only participates in topology (Kademlia/Hive).
    /// No storage, no availability accounting, no incentives.
    Bootnode,

    /// Light node - can retrieve chunks from the network.
    /// Requires availability accounting (pseudosettle or SWAP).
    #[default]
    Light,

    /// Publisher node - can retrieve + upload chunks.
    /// Requires availability accounting + postage stamps.
    Publisher,

    /// Full node - stores chunks for the network.
    /// Requires availability accounting + postage + pullsync.
    Full,

    /// Staker node - full storage with redistribution rewards.
    /// Requires everything from Full + staking + redistribution game.
    Staker,
}

impl NodeType {
    /// Check if this node type requires availability accounting.
    pub fn requires_availability(&self) -> bool {
        !matches!(self, NodeType::Bootnode)
    }

    /// Check if this node type requires retrieval protocol.
    pub fn requires_retrieval(&self) -> bool {
        !matches!(self, NodeType::Bootnode)
    }

    /// Check if this node type requires upload/postage.
    pub fn requires_upload(&self) -> bool {
        matches!(
            self,
            NodeType::Publisher | NodeType::Full | NodeType::Staker
        )
    }

    /// Check if this node type requires pullsync.
    pub fn requires_pullsync(&self) -> bool {
        matches!(self, NodeType::Full | NodeType::Staker)
    }

    /// Check if this node type requires local storage.
    pub fn requires_storage(&self) -> bool {
        matches!(self, NodeType::Full | NodeType::Staker)
    }

    /// Check if this node type requires redistribution.
    pub fn requires_redistribution(&self) -> bool {
        matches!(self, NodeType::Staker)
    }

    /// Check if this node type requires persistent identity.
    pub fn requires_persistent_identity(&self) -> bool {
        // Full/Staker need persistent overlay for storage responsibility.
        // Publisher does NOT need persistent identity - chunks are pre-signed
        // before upload, so the publisher just validates stamps are valid.
        matches!(self, NodeType::Full | NodeType::Staker)
    }

    /// Check if this node type requires persistent nonce (stable overlay).
    pub fn requires_persistent_nonce(&self) -> bool {
        // Only storage nodes need stable overlay address
        matches!(self, NodeType::Full | NodeType::Staker)
    }
}

// Keep NodeMode as an alias for backwards compatibility during migration
#[deprecated(note = "Use NodeType instead")]
pub type NodeMode = NodeType;

/// Configuration for the Vertex Swarm node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConfig {
    /// Node type (determines capabilities)
    #[serde(default)]
    pub node_type: NodeType,

    /// Identity configuration (nonce for overlay address)
    #[serde(default)]
    pub identity: IdentityConfig,

    /// Network configuration
    pub network: NetworkConfig,

    /// Storage configuration (only for Full/Staker nodes)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<StorageConfig>,

    /// Availability configuration (only for non-Bootnode types)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<AvailabilityConfig>,

    /// API configuration
    pub api: ApiConfig,
}

impl NodeConfig {
    /// Create a new default configuration for the given node type.
    pub fn new(network_spec: &Hive, node_type: NodeType) -> Self {
        Self {
            node_type,
            identity: IdentityConfig::default(),
            network: NetworkConfig {
                bootnodes: network_spec
                    .bootnodes
                    .iter()
                    .map(|addr| addr.to_string())
                    .collect(),
                ..Default::default()
            },
            storage: if node_type.requires_storage() {
                Some(StorageConfig {
                    redistribution: node_type == NodeType::Staker,
                    ..Default::default()
                })
            } else {
                None
            },
            availability: if node_type.requires_availability() {
                Some(AvailabilityConfig::default())
            } else {
                None
            },
            api: ApiConfig::default(),
        }
    }

    /// Load the configuration from the given path, or create a default one if it doesn't exist.
    pub fn load_or_create(
        path: impl AsRef<Path>,
        network_spec: &Hive,
        node_type: NodeType,
    ) -> Result<Self> {
        let path = path.as_ref();

        if path.exists() {
            let content = fs::read_to_string(path)?;
            let config: Self = toml::from_str(&content)?;
            Ok(config)
        } else {
            let config = Self::new(network_spec, node_type);
            config.save(path)?;
            Ok(config)
        }
    }

    /// Save the configuration to the given path.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let content = toml::to_string_pretty(self)?;
        fs::write(path, content)?;

        Ok(())
    }

    /// Apply command line arguments to override the configuration.
    pub fn apply_cli_args(
        &mut self,
        network_args: &NetworkArgs,
        storage_args: &StorageArgs,
        storage_incentive_args: &StorageIncentiveArgs,
        api_args: &ApiArgs,
        availability_args: &AvailabilityArgs,
        node_type: NodeType,
    ) {
        // Apply node type
        self.node_type = node_type;

        // Apply network args
        self.network.discovery = !network_args.disable_discovery;
        if let Some(bootnodes) = &network_args.bootnodes {
            self.network.bootnodes = bootnodes.clone();
        }
        self.network.addr = IpAddr::from_str(&network_args.addr).unwrap_or(self.network.addr);
        self.network.port = network_args.port;
        self.network.max_peers = network_args.max_peers;

        // Apply storage args only for storage nodes
        if node_type.requires_storage() {
            let storage = self.storage.get_or_insert_with(StorageConfig::default);
            storage.capacity_chunks = storage_args.capacity_chunks;
            storage.redistribution = storage_incentive_args.redistribution;
        } else {
            self.storage = None;
        }

        // Apply availability args only for non-bootnode types
        if node_type.requires_availability() {
            let availability = self
                .availability
                .get_or_insert_with(AvailabilityConfig::default);
            availability.pseudosettle_enabled = matches!(
                availability_args.mode,
                AvailabilityMode::Pseudosettle | AvailabilityMode::Both
            );
            availability.swap_enabled = matches!(
                availability_args.mode,
                AvailabilityMode::Swap | AvailabilityMode::Both
            );
            availability.payment_threshold = availability_args.payment_threshold;
            availability.payment_tolerance_percent = availability_args.payment_tolerance_percent;
            availability.base_price = availability_args.base_price;
            availability.refresh_rate = availability_args.refresh_rate;
            availability.early_payment_percent = availability_args.early_payment_percent;
            availability.light_factor = availability_args.light_factor;
        } else {
            self.availability = None;
        }

        // Apply API args
        self.api.grpc_enabled = api_args.grpc;
        self.api.grpc_addr = api_args.grpc_addr.clone();
        self.api.grpc_port = api_args.grpc_port;

        self.api.metrics_enabled = api_args.metrics;
        self.api.metrics_addr = api_args.metrics_addr.clone();
        self.api.metrics_port = api_args.metrics_port;
    }

    /// Get the gRPC server socket address.
    pub fn grpc_socket_addr(&self) -> SocketAddr {
        SocketAddr::new(
            IpAddr::from_str(&self.api.grpc_addr)
                .unwrap_or_else(|_| IpAddr::from_str(DEFAULT_LOCALHOST_ADDR).unwrap()),
            self.api.grpc_port,
        )
    }

    /// Get the metrics socket address.
    pub fn metrics_socket_addr(&self) -> SocketAddr {
        SocketAddr::new(
            IpAddr::from_str(&self.api.metrics_addr)
                .unwrap_or_else(|_| IpAddr::from_str(DEFAULT_LOCALHOST_ADDR).unwrap()),
            self.api.metrics_port,
        )
    }

    /// Get the P2P socket address.
    pub fn p2p_socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.network.addr, self.network.port)
    }
}
