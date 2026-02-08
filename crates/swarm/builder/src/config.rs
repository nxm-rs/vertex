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

    pub fn spec(&self) -> &Arc<Spec> {
        &self.spec
    }

    pub fn identity(&self) -> &Arc<Identity> {
        &self.identity
    }

    pub fn network(&self) -> &NetworkConfig<KademliaConfig> {
        &self.network
    }

    pub fn bandwidth(&self) -> &DefaultBandwidthConfig {
        &self.bandwidth
    }
}

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

    pub fn spec(&self) -> &Arc<Spec> {
        &self.spec
    }

    pub fn identity(&self) -> &Arc<Identity> {
        &self.identity
    }

    pub fn network(&self) -> &NetworkConfig<KademliaConfig> {
        &self.network
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

impl NodeBuildsProtocol for BootnodeConfig {
    type Protocol = SwarmProtocol<Self>;

    fn protocol_name(&self) -> &'static str {
        "Swarm Bootnode"
    }
}

impl NodeBuildsProtocol for ClientConfig {
    type Protocol = SwarmProtocol<Self>;

    fn protocol_name(&self) -> &'static str {
        "Swarm Client"
    }
}

impl NodeBuildsProtocol for StorerConfig {
    type Protocol = SwarmProtocol<Self>;

    fn protocol_name(&self) -> &'static str {
        "Swarm Storer"
    }
}
