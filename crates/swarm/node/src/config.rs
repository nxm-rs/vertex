//! Swarm protocol configuration (serializable Args layer).

use std::path::Path;
use std::sync::Arc;

use eyre::Result;
use serde::{Deserialize, Serialize};
use vertex_node_api::NodeProtocolConfig;
use vertex_swarm_bandwidth::{BandwidthArgs, BandwidthConfig, BandwidthConfigError};
use vertex_swarm_identity::{Identity, IdentityArgs};
use vertex_swarm_localstore::{LocalStoreArgs, LocalStoreConfig};
use vertex_swarm_primitives::SwarmNodeType;
use vertex_swarm_redistribution::{RedistributionArgs, StorageConfig};
use vertex_swarm_spec::Spec;

use vertex_swarm_api::ConfigError;

use crate::args::{NetworkArgs, NetworkConfig, ProtocolArgs};

/// Swarm protocol configuration (serializable Args layer).
///
/// Contains all Swarm-specific settings for config file serialization.
/// Used as the type parameter for `vertex_node_core::config::FullNodeConfig`.
/// Base price is a network-wide constant defined in the SwarmSpec.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProtocolConfig {
    pub node_type: SwarmNodeType,
    pub identity: IdentityArgs,
    pub network: NetworkArgs,
    pub bandwidth: BandwidthArgs,
    pub localstore: LocalStoreArgs,
    pub redistribution: RedistributionArgs,
}

impl ProtocolConfig {
    /// Create validated network configuration.
    pub fn network_config(&self) -> Result<NetworkConfig, ConfigError> {
        NetworkConfig::try_from(&self.network)
    }

    /// Create identity from keystore or ephemeral.
    pub fn identity(&self, spec: Arc<Spec>, network_dir: &Path) -> Result<Arc<Identity>> {
        self.identity.identity(spec, network_dir, self.node_type)
    }

    /// Create validated bandwidth accounting configuration.
    pub fn bandwidth_config(&self) -> Result<BandwidthConfig, BandwidthConfigError> {
        BandwidthConfig::try_from(&self.bandwidth)
    }

    /// Create local store configuration.
    pub fn local_store_config(&self) -> LocalStoreConfig {
        self.localstore.local_store_config()
    }

    /// Create storage incentives configuration.
    pub fn storage_config(&self) -> StorageConfig {
        self.redistribution.storage_config()
    }
}

impl ProtocolConfig {
    /// Set the node type.
    pub fn set_node_type(&mut self, node_type: SwarmNodeType) {
        self.node_type = node_type;
    }
}

impl NodeProtocolConfig for ProtocolConfig {
    type Args = ProtocolArgs;

    fn apply_args(&mut self, args: &Self::Args) {
        self.identity = args.identity.clone();
        self.network = args.network.clone();
        self.bandwidth = args.bandwidth.clone();
        self.localstore = args.localstore.clone();
        self.redistribution = args.redistribution.clone();
    }
}
