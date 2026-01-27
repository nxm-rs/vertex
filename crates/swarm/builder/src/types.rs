//! Default type implementations for Swarm nodes.
//!
//! This module provides concrete type implementations that wire together
//! the various Swarm components (identity, topology, accounting, etc.).
//!
//! These types use implementations from `vertex-client-*` crates.

use std::convert::Infallible;
use std::sync::Arc;

use async_trait::async_trait;
use vertex_bandwidth_core::Accounting;
use vertex_client_kademlia::KademliaTopology;
use vertex_node_types::NodeTypes;
use vertex_swarm_api::{
    BootnodeTypes, LightTypes, NetworkConfig, RunnableClientService, RunnableNode,
};
use vertex_swarm_core::{ClientHandle, ClientService, SwarmNode};
use vertex_swarm_identity::SwarmIdentity;
use vertex_swarmspec::Hive;

use crate::SwarmNodeError;

/// Wrapper that owns a SwarmNode and implements RunnableNode.
///
/// This adapts `SwarmNode::run(&mut self)` to `RunnableNode::run(self)`.
pub struct SwarmNodeRunner<N: vertex_swarm_api::SwarmNodeTypes> {
    node: SwarmNode<N>,
}

impl<N: vertex_swarm_api::SwarmNodeTypes> SwarmNodeRunner<N> {
    /// Create a new runner wrapping a SwarmNode.
    pub fn new(node: SwarmNode<N>) -> Self {
        Self { node }
    }
}

#[async_trait]
impl<N> RunnableNode for SwarmNodeRunner<N>
where
    N: vertex_swarm_api::SwarmNodeTypes,
{
    type Error = SwarmNodeError;

    async fn run(mut self) -> Result<(), Self::Error> {
        self.node
            .start_listening()
            .map_err(|e| SwarmNodeError::Launch(e.to_string()))?;
        self.node
            .connect_bootnodes()
            .await
            .map_err(|e| SwarmNodeError::Launch(e.to_string()))?;
        self.node.run().await.map_err(SwarmNodeError::Runtime)?;
        Ok(())
    }
}

/// Wrapper that owns a ClientService and implements RunnableClientService.
pub struct ClientServiceRunner {
    service: ClientService,
}

impl ClientServiceRunner {
    /// Create a new runner wrapping a ClientService.
    pub fn new(service: ClientService) -> Self {
        Self { service }
    }
}

#[async_trait]
impl RunnableClientService for ClientServiceRunner {
    type Error = Infallible;

    async fn run(self) -> Result<(), Self::Error> {
        self.service.run().await;
        Ok(())
    }
}

/// Default types for light nodes.
///
/// This single type satisfies both capability traits (`LightTypes`) and
/// infrastructure traits (`SwarmLightNodeTypes` via blanket impl).
///
/// Concrete implementations used:
/// - `SwarmIdentity` for identity
/// - `KademliaTopology` for topology
/// - `Accounting` for bandwidth accounting
/// - `SwarmNodeRunner` as the node event loop
/// - `ClientServiceRunner` as the client service
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
    type Node = SwarmNodeRunner<Self>;
    type ClientService = ClientServiceRunner;
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
