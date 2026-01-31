//! Default type implementations for Swarm nodes.
//!
//! This module provides concrete type implementations that wire together
//! the various Swarm components (identity, topology, accounting, etc.).
//!
//! These types use implementations from `vertex-client-*` crates.

use std::sync::Arc;

use vertex_swarm_bandwidth::Accounting;
use vertex_swarm_kademlia::KademliaTopology;
use vertex_node_types::NodeTypes;
use vertex_swarm_api::{SwarmBootnodeTypes, SwarmClientTypes, SwarmNetworkConfig};
use vertex_tasks::SpawnableTask;
use vertex_swarm_core::{BootNode, ClientHandle, ClientService, SwarmNode};
use vertex_swarm_identity::Identity;
use vertex_swarmspec::Hive;

use crate::components::DefaultAccountingConfig;

/// Default types for light nodes.
///
/// This single type satisfies both capability traits (`SwarmClientTypes`) and
/// infrastructure traits (`SwarmLightNodeTypes` via blanket impl).
///
/// Concrete implementations used:
/// - `Identity` for identity
/// - `KademliaTopology` for topology
/// - `Accounting<DefaultAccountingConfig>` for bandwidth accounting
/// - `SwarmNode` as the node event loop
/// - `ClientService` as the client service
#[derive(Debug, Clone)]
pub struct DefaultClientTypes;

impl NodeTypes for DefaultClientTypes {
    type Database = ();
    type Rpc = ();
    type Executor = ();
}

impl SwarmBootnodeTypes for DefaultClientTypes {
    type Spec = Hive;
    type Identity = Arc<Identity>;
    type Topology = Arc<KademliaTopology<Arc<Identity>>>;
    type Node = SwarmNode<Self>;
    type ClientService = ClientService;
    type ClientHandle = ClientHandle;
}

impl SwarmClientTypes for DefaultClientTypes {
    type Accounting = Arc<Accounting<DefaultAccountingConfig, Arc<Identity>>>;
}

/// Default types for bootnodes.
///
/// Unlike light nodes, bootnodes don't need client protocols. They use
/// `BootNode` which only has topology behaviour (handshake, hive, pingpong).
#[derive(Debug, Clone)]
pub struct DefaultBootnodeTypes;

impl NodeTypes for DefaultBootnodeTypes {
    type Database = ();
    type Rpc = ();
    type Executor = ();
}

/// No-op client service for bootnodes (they don't have client protocols).
pub struct NoOpClientService;

impl SpawnableTask for NoOpClientService {
    fn into_task(self) -> impl std::future::Future<Output = ()> + Send {
        async {}
    }
}

/// No-op client handle for bootnodes.
#[derive(Clone)]
pub struct NoOpClientHandle;

impl SwarmBootnodeTypes for DefaultBootnodeTypes {
    type Spec = Hive;
    type Identity = Arc<Identity>;
    type Topology = Arc<KademliaTopology<Arc<Identity>>>;
    type Node = BootNode<Self>;
    type ClientService = NoOpClientService;
    type ClientHandle = NoOpClientHandle;
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
    /// NAT/external addresses to advertise.
    pub nat_addrs: Vec<String>,
    /// Whether auto-NAT discovery is enabled.
    pub nat_auto: bool,
}

impl Default for DefaultNetworkConfig {
    fn default() -> Self {
        Self {
            listen_addrs: vec!["/ip4/0.0.0.0/tcp/1634".to_string()],
            bootnodes: vec![],
            discovery_enabled: true,
            max_peers: 50,
            idle_timeout_secs: 30,
            nat_addrs: vec![],
            nat_auto: false,
        }
    }
}

impl SwarmNetworkConfig for DefaultNetworkConfig {
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

    fn nat_addrs(&self) -> Vec<String> {
        self.nat_addrs.clone()
    }

    fn nat_auto_enabled(&self) -> bool {
        self.nat_auto
    }
}
