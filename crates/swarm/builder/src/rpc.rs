//! RPC providers for Swarm nodes.

use vertex_rpc_server::{GrpcRegistry, RegistersGrpcServices};
use vertex_swarm_api::{HasTopology, SwarmChunkProvider, SwarmChunkSender, SwarmIdentity};
use vertex_swarm_rpc::{ChunkService, NodeService, proto};
use vertex_swarm_topology::TopologyHandle;

/// Register the node status service and the reflection descriptor that every
/// node type's gRPC surface shares.
fn register_node_service<I: SwarmIdentity>(
    registry: &mut GrpcRegistry,
    topology: &TopologyHandle<I>,
) {
    let node_service = NodeService::new(topology.clone());
    let node_server = proto::node::node_server::NodeServer::new(node_service);
    registry.add_service(node_server);

    registry.add_descriptor(proto::FILE_DESCRIPTOR_SET);
}

/// Register the chunk upload/download service that backs client and storer
/// nodes.
fn register_chunk_service<C>(registry: &mut GrpcRegistry, chunks: &C)
where
    C: SwarmChunkProvider + SwarmChunkSender + Clone,
{
    let chunk_service = ChunkService::new(chunks.clone());
    let chunk_server = proto::chunk::chunk_server::ChunkServer::new(chunk_service);
    registry.add_service(chunk_server);
}

/// RPC providers for client nodes (topology status + chunk retrieval).
pub struct ClientRpcProviders<I: SwarmIdentity, C> {
    topology: TopologyHandle<I>,
    chunks: C,
}

impl<I: SwarmIdentity, C> ClientRpcProviders<I, C> {
    /// Create providers from the topology handle and chunk provider of a built
    /// client node.
    pub fn new(topology: TopologyHandle<I>, chunks: C) -> Self {
        Self { topology, chunks }
    }

    /// Access the chunk provider that backs uploads and downloads.
    ///
    /// Embedders that drive a client directly (FFI, gRPC, or an example) borrow
    /// this to call [`SwarmChunkSender`] and [`SwarmChunkProvider`] without going
    /// through the gRPC surface.
    pub fn chunks(&self) -> &C {
        &self.chunks
    }
}

impl<I: SwarmIdentity, C: Send + Sync> HasTopology for ClientRpcProviders<I, C> {
    type Topology = TopologyHandle<I>;

    fn topology(&self) -> &TopologyHandle<I> {
        &self.topology
    }
}

impl<I: SwarmIdentity, C: SwarmChunkProvider + SwarmChunkSender + Clone> RegistersGrpcServices
    for ClientRpcProviders<I, C>
{
    fn register_grpc_services(&self, registry: &mut GrpcRegistry) {
        register_node_service(registry, &self.topology);
        register_chunk_service(registry, &self.chunks);
    }
}

/// RPC providers for bootnodes (topology status only).
pub struct BootnodeRpcProviders<I: SwarmIdentity> {
    topology: TopologyHandle<I>,
}

impl<I: SwarmIdentity> BootnodeRpcProviders<I> {
    /// Create providers from the topology handle of a built bootnode.
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
        register_node_service(registry, &self.topology);
    }
}

/// RPC providers for storer (full) nodes (topology + chunks + storage).
pub struct StorerRpcProviders<I: SwarmIdentity, C> {
    topology: TopologyHandle<I>,
    chunks: C,
}

impl<I: SwarmIdentity, C> StorerRpcProviders<I, C> {
    /// Create providers from the topology handle and chunk provider of a built
    /// storer node.
    pub fn new(topology: TopologyHandle<I>, chunks: C) -> Self {
        Self { topology, chunks }
    }
}

impl<I: SwarmIdentity, C: Send + Sync> HasTopology for StorerRpcProviders<I, C> {
    type Topology = TopologyHandle<I>;

    fn topology(&self) -> &TopologyHandle<I> {
        &self.topology
    }
}

impl<I: SwarmIdentity, C: SwarmChunkProvider + SwarmChunkSender + Clone> RegistersGrpcServices
    for StorerRpcProviders<I, C>
{
    fn register_grpc_services(&self, registry: &mut GrpcRegistry) {
        register_node_service(registry, &self.topology);
        register_chunk_service(registry, &self.chunks);

        // TODO: Add storer-specific RPC services (storage, redistribution, etc.)
    }
}
