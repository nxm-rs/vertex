//! Swarm protocol configuration.
//!
//! [`SwarmConfig`] contains all Swarm-specific configuration settings.
//! It implements [`ProtocolConfig`] for use with the generic
//! [`vertex_node_core::config::FullNodeConfig`].
//!
//! # Example
//!
//! ```ignore
//! use vertex_node_core::config::FullNodeConfig;
//! use vertex_swarm_core::SwarmConfig;
//!
//! // Load combined config (infrastructure + Swarm protocol)
//! let config = FullNodeConfig::<SwarmConfig>::load(Some(&path))?;
//!
//! // Access Swarm-specific settings
//! println!("Node type: {:?}", config.protocol.node_type);
//! println!("Max peers: {}", config.protocol.network.max_peers);
//! ```

use serde::{Deserialize, Serialize};
use vertex_node_api::NodeProtocolConfig;

use vertex_swarm_primitives::{BandwidthMode, SwarmNodeType};

use crate::args::{
    BandwidthArgs, IdentityArgs, NetworkArgs, StorageArgs, StorageIncentiveArgs, SwarmArgs,
};

/// Swarm protocol configuration.
///
/// Contains all Swarm-specific settings, separate from generic node
/// infrastructure configuration. This struct is used as the type parameter
/// for [`vertex_node_core::config::FullNodeConfig`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SwarmConfig {
    /// Node type (determines capabilities).
    pub node_type: SwarmNodeType,

    /// Network configuration.
    pub network: NetworkArgs,

    /// Bandwidth incentive configuration.
    pub bandwidth: BandwidthArgs,

    /// Storage configuration.
    pub storage: StorageArgs,

    /// Storage incentive configuration.
    pub storage_incentives: StorageIncentiveArgs,

    /// Identity configuration.
    pub identity: IdentityArgs,
}

impl Default for SwarmConfig {
    fn default() -> Self {
        Self {
            node_type: SwarmNodeType::default(),
            network: NetworkArgs::default(),
            bandwidth: BandwidthArgs::default(),
            storage: StorageArgs::default(),
            storage_incentives: StorageIncentiveArgs::default(),
            identity: IdentityArgs::default(),
        }
    }
}

impl NodeProtocolConfig for SwarmConfig {
    type Args = SwarmArgs;

    fn apply_args(&mut self, args: &Self::Args) {
        self.node_type = args.node_type.into();
        self.network = args.network.clone();
        self.bandwidth = args.bandwidth.clone();
        self.storage = args.storage.clone();
        self.storage_incentives = args.storage_incentives.clone();
        self.identity = args.identity.clone();
    }
}

impl SwarmConfig {
    /// Get the P2P listen address as a multiaddr string.
    pub fn p2p_listen_multiaddr(&self) -> String {
        self.network.listen_multiaddr()
    }

    /// Get the bandwidth mode.
    pub fn bandwidth_mode(&self) -> BandwidthMode {
        self.bandwidth.mode.into()
    }

    /// Returns true if the node type requires a persistent identity.
    pub fn requires_persistent_identity(&self) -> bool {
        self.node_type.requires_persistent_identity()
    }
}
