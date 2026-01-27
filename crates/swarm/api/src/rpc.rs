//! RPC providers for Swarm protocol.
//!
//! This module defines the container struct for RPC data sources that can
//! be wired into RPC services (gRPC, JSON-RPC, etc.).

use crate::TopologyProvider;

/// RPC providers for the Swarm protocol.
///
/// This struct contains all the data sources that can be exposed via RPC.
/// It is constructed from [`SwarmComponents`](crate::SwarmComponents) and
/// passed to RPC service providers.
///
/// # Example
///
/// ```ignore
/// use vertex_swarm_api::{SwarmProtocol, SwarmRpcProviders};
/// use vertex_node_api::Protocol;
///
/// let providers = SwarmProtocol::providers(&components);
///
/// // Wire into gRPC (via vertex-swarm-rpc)
/// let router = providers.register_grpc_services(server_builder);
/// ```
#[derive(Debug, Clone)]
pub struct SwarmRpcProviders<Topo> {
    /// Topology provider for network status information.
    pub topology: Topo,
    // Future providers:
    // pub accounting: Acct,
    // pub storage: Store,
}

impl<Topo> SwarmRpcProviders<Topo> {
    /// Create new RPC providers.
    pub fn new(topology: Topo) -> Self {
        Self { topology }
    }
}

impl<Topo: TopologyProvider> SwarmRpcProviders<Topo> {
    /// Get reference to the topology provider.
    pub fn topology(&self) -> &Topo {
        &self.topology
    }
}
