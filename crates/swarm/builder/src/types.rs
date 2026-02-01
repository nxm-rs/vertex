//! Default type implementations for Swarm nodes.
//!
//! This module provides concrete type implementations that wire together
//! the various Swarm components (identity, topology, accounting, etc.).

use std::sync::Arc;

use vertex_swarm_bandwidth::{Accounting, ClientAccounting, DefaultAccountingConfig, FixedPricer};
use vertex_swarm_kademlia::KademliaTopology;
use vertex_node_types::NodeTypes;
use vertex_swarm_api::{SwarmBootnodeTypes, SwarmClientTypes, SwarmNetworkConfig};
use vertex_swarm_identity::Identity;
use vertex_swarmspec::Hive;

/// Default types for client nodes.
///
/// Concrete implementations:
/// - `Identity` for cryptographic identity
/// - `KademliaTopology` for peer discovery
/// - `ClientAccounting` for bandwidth incentives
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
}

impl SwarmClientTypes for DefaultClientTypes {
    type Accounting = ClientAccounting<Arc<Accounting<DefaultAccountingConfig, Arc<Identity>>>, FixedPricer>;
}

/// Default types for bootnodes.
///
/// Bootnodes only participate in topology (no chunk retrieval).
#[derive(Debug, Clone)]
pub struct DefaultBootnodeTypes;

impl NodeTypes for DefaultBootnodeTypes {
    type Database = ();
    type Rpc = ();
    type Executor = ();
}

impl SwarmBootnodeTypes for DefaultBootnodeTypes {
    type Spec = Hive;
    type Identity = Arc<Identity>;
    type Topology = Arc<KademliaTopology<Arc<Identity>>>;
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
