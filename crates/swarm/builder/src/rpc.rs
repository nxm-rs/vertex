//! RPC providers for Swarm nodes.

use vertex_rpc_server::{GrpcRegistry, RegistersGrpcServices};
use vertex_swarm_api::{HasTopology, SwarmChunkProvider, SwarmIdentity};
use vertex_swarm_rpc::{ChunkService, NodeService, proto};
use vertex_swarm_topology::TopologyHandle;

/// RPC providers for non-bootnode nodes (topology status + chunk retrieval).
///
/// Used by both client and storer nodes. Storer-specific RPC services
/// will be added when storer components are implemented.
pub struct FullRpcProviders<I: SwarmIdentity, C> {
    topology: TopologyHandle<I>,
    chunks: C,
}

impl<I: SwarmIdentity, C> FullRpcProviders<I, C> {
    pub fn new(topology: TopologyHandle<I>, chunks: C) -> Self {
        Self { topology, chunks }
    }
}

impl<I: SwarmIdentity, C: Send + Sync> HasTopology for FullRpcProviders<I, C> {
    type Topology = TopologyHandle<I>;

    fn topology(&self) -> &TopologyHandle<I> {
        &self.topology
    }
}

impl<I: SwarmIdentity, C: SwarmChunkProvider + Clone> RegistersGrpcServices
    for FullRpcProviders<I, C>
{
    fn register_grpc_services(&self, registry: &mut GrpcRegistry) {
        let node_service = NodeService::new(self.topology.clone());
        let node_server = proto::node::node_server::NodeServer::new(node_service);
        registry.add_service(node_server);

        let chunk_service = ChunkService::new(self.chunks.clone());
        let chunk_server = proto::chunk::chunk_server::ChunkServer::new(chunk_service);
        registry.add_service(chunk_server);

        registry.add_descriptor(proto::FILE_DESCRIPTOR_SET);
    }
}

/// RPC providers for bootnodes (topology status only).
pub struct BootnodeRpcProviders<I: SwarmIdentity> {
    topology: TopologyHandle<I>,
}

impl<I: SwarmIdentity> BootnodeRpcProviders<I> {
    pub fn new(topology: TopologyHandle<I>) -> Self {
        Self { topology }
    }
}

impl<I: SwarmIdentity> HasTopology for BootnodeRpcProviders<I> {
    type Topology = TopologyHandle<I>;

    fn topology(&self) -> &TopologyHandle<I> {
        &self.topology
    }
}

impl<I: SwarmIdentity> RegistersGrpcServices for BootnodeRpcProviders<I> {
    fn register_grpc_services(&self, registry: &mut GrpcRegistry) {
        let node_service = NodeService::new(self.topology.clone());
        let node_server = proto::node::node_server::NodeServer::new(node_service);
        registry.add_service(node_server);

        registry.add_descriptor(proto::FILE_DESCRIPTOR_SET);
    }
}

/// Unified RPC providers for runtime node-type dispatch.
///
/// Wraps the bootnode and full-node provider types so that
/// [`SwarmNodeConfig`](crate::SwarmNodeConfig) can have a single `Providers`
/// associated type.
pub enum SwarmNodeProviders {
    /// Bootnode providers (topology only).
    Bootnode(BootnodeRpcProviders<std::sync::Arc<vertex_swarm_identity::Identity>>),
    /// Full-node providers (topology + chunk retrieval).
    Full(
        FullRpcProviders<
            std::sync::Arc<vertex_swarm_identity::Identity>,
            crate::NetworkChunkProvider<std::sync::Arc<vertex_swarm_identity::Identity>>,
        >,
    ),
}

impl RegistersGrpcServices for SwarmNodeProviders {
    fn register_grpc_services(&self, registry: &mut GrpcRegistry) {
        match self {
            Self::Bootnode(p) => p.register_grpc_services(registry),
            Self::Full(p) => p.register_grpc_services(registry),
        }
    }
}
