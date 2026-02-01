//! Swarm protocol configuration.

use serde::{Deserialize, Serialize};
use vertex_node_api::NodeProtocolConfig;
use vertex_swarm_bandwidth::BandwidthArgs;
use vertex_swarm_bandwidth_pricing::PricingArgs;
use vertex_swarm_identity::IdentityArgs;
use vertex_swarm_localstore::LocalStoreArgs;
use vertex_swarm_primitives::{BandwidthMode, SwarmNodeType};

use crate::args::{NetworkArgs, ProtocolArgs, StorageIncentiveArgs};

/// Swarm protocol configuration.
///
/// Contains all Swarm-specific settings. Used as the type parameter
/// for `vertex_node_core::config::FullNodeConfig`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SwarmConfig {
    pub node_type: SwarmNodeType,
    pub network: NetworkArgs,
    pub bandwidth: BandwidthArgs,
    pub pricing: PricingArgs,
    pub localstore: LocalStoreArgs,
    pub storage_incentives: StorageIncentiveArgs,
    pub identity: IdentityArgs,
}

impl NodeProtocolConfig for SwarmConfig {
    type Args = ProtocolArgs;

    fn apply_args(&mut self, args: &Self::Args) {
        self.node_type = args.node_type.into();
        self.network = args.network.clone();
        self.bandwidth = args.bandwidth.clone();
        self.pricing = args.pricing.clone();
        self.localstore = args.localstore.clone();
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
