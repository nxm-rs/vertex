//! Node-type-specific validated configurations holding runtime objects.

use std::sync::Arc;

use vertex_node_api::NodeBuildsProtocol;
use vertex_swarm_api::SwarmProtocol;
use vertex_swarm_bandwidth::DefaultBandwidthConfig;
use vertex_swarm_identity::Identity;
use vertex_swarm_localstore::LocalStoreConfig;
use vertex_swarm_node::args::NetworkConfig;
use vertex_swarm_redistribution::StorageConfig;
use vertex_swarm_spec::Spec;
use vertex_swarm_topology::KademliaConfig;

/// Implement shared config getters: `spec()`, `identity()`, `network()`.
macro_rules! impl_common_config_getters {
    ($ty:ident) => {
        impl $ty {
            pub fn spec(&self) -> &Arc<Spec> {
                &self.spec
            }

            pub fn identity(&self) -> &Arc<Identity> {
                &self.identity
            }

            pub fn network(&self) -> &NetworkConfig<KademliaConfig> {
                &self.network
            }
        }
    };
}

/// Implement `NodeBuildsProtocol` with a given protocol name.
macro_rules! impl_builds_protocol {
    ($ty:ident, $name:expr) => {
        impl NodeBuildsProtocol for $ty {
            type Protocol = SwarmProtocol<Self>;

            fn protocol_name(&self) -> &'static str {
                $name
            }
        }
    };
}

/// Validated configuration for bootnode (network identity and topology only).
#[derive(Clone)]
pub struct BootnodeConfig {
    spec: Arc<Spec>,
    identity: Arc<Identity>,
    network: NetworkConfig<KademliaConfig>,
}

impl BootnodeConfig {
    pub fn new(
        spec: Arc<Spec>,
        identity: Arc<Identity>,
        network: NetworkConfig<KademliaConfig>,
    ) -> Self {
        Self {
            spec,
            identity,
            network,
        }
    }
}

impl_common_config_getters!(BootnodeConfig);
impl_builds_protocol!(BootnodeConfig, "Swarm Bootnode");

/// Validated configuration for client (light) node with bandwidth accounting.
#[derive(Clone)]
pub struct ClientConfig {
    spec: Arc<Spec>,
    identity: Arc<Identity>,
    network: NetworkConfig<KademliaConfig>,
    bandwidth: DefaultBandwidthConfig,
}

impl ClientConfig {
    pub fn new(
        spec: Arc<Spec>,
        identity: Arc<Identity>,
        network: NetworkConfig<KademliaConfig>,
        bandwidth: DefaultBandwidthConfig,
    ) -> Self {
        Self {
            spec,
            identity,
            network,
            bandwidth,
        }
    }

    pub fn bandwidth(&self) -> &DefaultBandwidthConfig {
        &self.bandwidth
    }
}

impl_common_config_getters!(ClientConfig);
impl_builds_protocol!(ClientConfig, "Swarm Client");

/// Validated configuration for storer (full) node with storage and redistribution.
#[derive(Clone)]
pub struct StorerConfig {
    spec: Arc<Spec>,
    identity: Arc<Identity>,
    network: NetworkConfig<KademliaConfig>,
    bandwidth: DefaultBandwidthConfig,
    local_store: LocalStoreConfig,
    storage: StorageConfig,
}

impl StorerConfig {
    pub fn new(
        spec: Arc<Spec>,
        identity: Arc<Identity>,
        network: NetworkConfig<KademliaConfig>,
        bandwidth: DefaultBandwidthConfig,
        local_store: LocalStoreConfig,
        storage: StorageConfig,
    ) -> Self {
        Self {
            spec,
            identity,
            network,
            bandwidth,
            local_store,
            storage,
        }
    }

    pub fn bandwidth(&self) -> &DefaultBandwidthConfig {
        &self.bandwidth
    }

    pub fn local_store(&self) -> &LocalStoreConfig {
        &self.local_store
    }

    pub fn storage(&self) -> &StorageConfig {
        &self.storage
    }
}

impl_common_config_getters!(StorerConfig);
impl_builds_protocol!(StorerConfig, "Swarm Storer");
