//! Node-type-specific validated configurations holding runtime objects.

use std::path::PathBuf;
use std::sync::Arc;

use vertex_swarm_bandwidth::DefaultBandwidthConfig;
use vertex_swarm_identity::Identity;
use vertex_swarm_localstore::LocalStoreConfig;
use vertex_swarm_node::args::NetworkConfig;
use vertex_swarm_redistribution::StorageConfig;
use vertex_swarm_spec::Spec;

/// Validated configuration for bootnode (network identity and topology only).
#[derive(Clone)]
pub struct BootnodeConfig {
    pub spec: Arc<Spec>,
    pub identity: Arc<Identity>,
    pub network: NetworkConfig,
    pub peers_path: PathBuf,
}

impl BootnodeConfig {
    pub fn new(
        spec: Arc<Spec>,
        identity: Arc<Identity>,
        network: NetworkConfig,
        peers_path: PathBuf,
    ) -> Self {
        Self {
            spec,
            identity,
            network,
            peers_path,
        }
    }
}

/// Validated configuration for client (light) node with bandwidth accounting.
#[derive(Clone)]
pub struct ClientConfig {
    pub spec: Arc<Spec>,
    pub identity: Arc<Identity>,
    pub network: NetworkConfig,
    pub bandwidth: DefaultBandwidthConfig,
    pub peers_path: PathBuf,
}

impl ClientConfig {
    pub fn new(
        spec: Arc<Spec>,
        identity: Arc<Identity>,
        network: NetworkConfig,
        bandwidth: DefaultBandwidthConfig,
        peers_path: PathBuf,
    ) -> Self {
        Self {
            spec,
            identity,
            network,
            bandwidth,
            peers_path,
        }
    }
}

/// Validated configuration for storer (full) node with storage and redistribution.
#[derive(Clone)]
pub struct StorerConfig {
    pub spec: Arc<Spec>,
    pub identity: Arc<Identity>,
    pub network: NetworkConfig,
    pub bandwidth: DefaultBandwidthConfig,
    pub local_store: LocalStoreConfig,
    pub storage: StorageConfig,
    pub peers_path: PathBuf,
}

impl StorerConfig {
    pub fn new(
        spec: Arc<Spec>,
        identity: Arc<Identity>,
        network: NetworkConfig,
        bandwidth: DefaultBandwidthConfig,
        local_store: LocalStoreConfig,
        storage: StorageConfig,
        peers_path: PathBuf,
    ) -> Self {
        Self {
            spec,
            identity,
            network,
            bandwidth,
            local_store,
            storage,
            peers_path,
        }
    }
}
