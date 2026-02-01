//! Swarm protocol configuration.

use serde::{Deserialize, Serialize};
use vertex_node_api::NodeProtocolConfig;
use vertex_swarm_bandwidth::BandwidthArgs;
use vertex_swarm_bandwidth_pricing::PricingArgs;
use vertex_swarm_identity::IdentityArgs;
use vertex_swarm_localstore::LocalStoreArgs;
use vertex_swarm_primitives::SwarmNodeType;
use vertex_swarm_redistribution::RedistributionArgs;

use crate::args::{NetworkArgs, ProtocolArgs};

/// Swarm protocol configuration.
///
/// Contains all Swarm-specific settings. Used as the type parameter
/// for `vertex_node_core::config::FullNodeConfig`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProtocolConfig {
    pub node_type: SwarmNodeType,
    pub network: NetworkArgs,
    pub bandwidth: BandwidthArgs,
    pub pricing: PricingArgs,
    pub localstore: LocalStoreArgs,
    pub redistribution: RedistributionArgs,
    pub identity: IdentityArgs,
}

impl NodeProtocolConfig for ProtocolConfig {
    type Args = ProtocolArgs;

    fn apply_args(&mut self, args: &Self::Args) {
        self.node_type = args.node_type.into();
        self.network = args.network.clone();
        self.bandwidth = args.bandwidth.clone();
        self.pricing = args.pricing.clone();
        self.localstore = args.localstore.clone();
        self.redistribution = args.redistribution.clone();
        self.identity = args.identity.clone();
    }
}
