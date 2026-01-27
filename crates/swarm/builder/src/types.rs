//! Default type implementations for Swarm nodes.
//!
//! This module provides concrete type implementations that wire together
//! the various Swarm components (identity, topology, accounting, etc.).
//!
//! These types use implementations from `vertex-client-*` crates.

use std::sync::Arc;

use vertex_bandwidth_core::Accounting;
use vertex_client_kademlia::KademliaTopology;
use vertex_node_types::NodeTypes;
use vertex_swarm_api::{BootnodeTypes, LightTypes, NetworkConfig};
use vertex_swarm_core::{ClientHandle, ClientService, SwarmNode};
use vertex_swarm_identity::SwarmIdentity;
use vertex_swarmspec::Hive;

/// Default types for light nodes.
///
/// This single type satisfies both capability traits (`LightTypes`) and
/// infrastructure traits (`SwarmLightNodeTypes` via blanket impl).
///
/// Concrete implementations used:
/// - `SwarmIdentity` for identity
/// - `KademliaTopology` for topology
/// - `Accounting` for bandwidth accounting
/// - `SwarmNode` as the node event loop (implements `SpawnableTask`)
/// - `ClientService` as the client service (implements `SpawnableTask`)
#[derive(Debug, Clone)]
pub struct DefaultLightTypes;

impl NodeTypes for DefaultLightTypes {
    type Database = ();
    type Rpc = ();
    type Executor = ();
}

impl BootnodeTypes for DefaultLightTypes {
    type Spec = Hive;
    type Identity = Arc<SwarmIdentity>;
    type Topology = Arc<KademliaTopology<Arc<SwarmIdentity>>>;
    type Node = SwarmNode<Self>;
    type ClientService = ClientService;
    type ClientHandle = ClientHandle;
}

impl LightTypes for DefaultLightTypes {
    type Accounting = Arc<Accounting>;
}

/// Default network configuration.
#[derive(Debug, Clone)]
pub struct DefaultNetworkConfig {
    /// Listen addresses.
    pub listen_addrs: Vec<String>,
    /// Bootnode addresses.
    pub bootnodes: Vec<String>,
    /// Whether discovery is enabled.
    pub discovery_enabled: bool,
    /// Maximum peers.
    pub max_peers: usize,
    /// Idle timeout in seconds.
    pub idle_timeout_secs: u64,
}

impl Default for DefaultNetworkConfig {
    fn default() -> Self {
        Self {
            listen_addrs: vec!["/ip4/0.0.0.0/tcp/1634".to_string()],
            bootnodes: vec![],
            discovery_enabled: true,
            max_peers: 50,
            idle_timeout_secs: 30,
        }
    }
}

impl NetworkConfig for DefaultNetworkConfig {
    fn listen_addrs(&self) -> Vec<String> {
        self.listen_addrs.clone()
    }

    fn bootnodes(&self) -> Vec<String> {
        self.bootnodes.clone()
    }

    fn discovery_enabled(&self) -> bool {
        self.discovery_enabled
    }

    fn max_peers(&self) -> usize {
        self.max_peers
    }

    fn idle_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.idle_timeout_secs)
    }
}
