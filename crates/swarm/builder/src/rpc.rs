//! RPC providers for Swarm nodes.

use vertex_rpc_server::{GrpcRegistry, RegistersGrpcServices};
use vertex_swarm_api::{HasTopology, SwarmChunkProvider, SwarmIdentity};
use vertex_swarm_rpc::{ChunkService, NodeService, proto};
use vertex_swarm_topology::TopologyHandle;

/// RPC providers for client nodes (topology status + chunk retrieval).
pub struct ClientRpcProviders<I: SwarmIdentity, C> {
    topology: TopologyHandle<I>,
    chunks: C,
}

impl<I: SwarmIdentity, C> ClientRpcProviders<I, C> {
    pub fn new(topology: TopologyHandle<I>, chunks: C) -> Self {
        Self { topology, chunks }
    }
}

impl<I: SwarmIdentity, C: Send + Sync> HasTopology for ClientRpcProviders<I, C> {
    type Topology = TopologyHandle<I>;

    fn topology(&self) -> &TopologyHandle<I> {
        &self.topology
    }
}

impl<I: SwarmIdentity, C: SwarmChunkProvider + Clone> RegistersGrpcServices
    for ClientRpcProviders<I, C>
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

/// RPC providers for storer (full) nodes (topology + chunks + storage).
pub struct StorerRpcProviders<I: SwarmIdentity> {
    topology: TopologyHandle<I>,
}

impl<I: SwarmIdentity> StorerRpcProviders<I> {
    pub fn new(topology: TopologyHandle<I>) -> Self {
        Self { topology }
    }
}

impl<I: SwarmIdentity> HasTopology for StorerRpcProviders<I> {
    type Topology = TopologyHandle<I>;

    fn topology(&self) -> &TopologyHandle<I> {
        &self.topology
    }
}

impl<I: SwarmIdentity> RegistersGrpcServices for StorerRpcProviders<I> {
    fn register_grpc_services(&self, registry: &mut GrpcRegistry) {
        // TODO: Add storer-specific RPC services (storage, redistribution, etc.)
        let node_service = NodeService::new(self.topology.clone());
        let node_server = proto::node::node_server::NodeServer::new(node_service);
        registry.add_service(node_server);

        registry.add_descriptor(proto::FILE_DESCRIPTOR_SET);
    }
}
