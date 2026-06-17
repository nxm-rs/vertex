//! gRPC adapter wrapping an api component container.
//!
//! [`GrpcAdapter<C>`] is the gRPC transport's view of a built node: it wraps an
//! api component container `C` and registers exactly the services the container's
//! capabilities expose. The node status service is gated on [`HasTopology`]; the
//! chunk service is gated on [`HasChunkClient`]. Registration is driven through
//! per-shape [`RegistersGrpcServices`] impls (one per concrete container) so the
//! optional chunk capability never produces overlapping blanket impls.
//!
//! The adapter is constructed only at `bin/vertex`, the gRPC selection point.

use vertex_rpc_server::{GrpcRegistry, RegistersGrpcServices};
use vertex_swarm_api::{
    BootnodeComponents, ClientComponents, HasChunkClient, HasTopology, SwarmTopologyPeers,
    SwarmTopologyState, SwarmTopologyStats,
};

use crate::{ChunkService, ChunkServiceProvider, NodeService, proto};

/// gRPC transport adapter over an api component container `C`.
///
/// The gRPC surface is driven by the capabilities `C` exposes. Capability access
/// ([`HasTopology`], [`HasChunkClient`]) delegates to `C`.
#[derive(Debug, Clone)]
pub struct GrpcAdapter<C> {
    components: C,
}

impl<C> GrpcAdapter<C> {
    /// Wrap a components container.
    pub fn new(components: C) -> Self {
        Self { components }
    }

    /// Access the wrapped components.
    pub fn components(&self) -> &C {
        &self.components
    }

    /// Consume and return the wrapped components.
    pub fn into_components(self) -> C {
        self.components
    }
}

impl<C: HasTopology> HasTopology for GrpcAdapter<C> {
    type Topology = C::Topology;

    fn topology(&self) -> &Self::Topology {
        self.components.topology()
    }
}

impl<C: HasChunkClient> HasChunkClient for GrpcAdapter<C> {
    type ChunkClient = C::ChunkClient;

    fn chunk_client(&self) -> &Self::ChunkClient {
        self.components.chunk_client()
    }
}

impl<C> GrpcAdapter<C> {
    /// Register the node status service and the shared reflection descriptor.
    ///
    /// Gated on [`HasTopology`]: any container carrying a topology that satisfies
    /// the node service bounds can expose status and topology queries.
    pub fn register_node(&self, registry: &mut GrpcRegistry)
    where
        C: HasTopology,
        C::Topology: SwarmTopologyState
            + SwarmTopologyStats
            + SwarmTopologyPeers
            + Clone
            + Send
            + Sync
            + 'static,
    {
        let node_service = NodeService::new(self.components.topology().clone());
        let node_server = proto::node::node_server::NodeServer::new(node_service);
        registry.add_service(node_server);
        registry.add_descriptor(proto::FILE_DESCRIPTOR_SET);
    }

    /// Register the chunk upload/download service.
    ///
    /// Gated on [`HasChunkClient`]: only containers carrying a chunk client
    /// expose the chunk service.
    pub fn register_chunk(&self, registry: &mut GrpcRegistry)
    where
        C: HasChunkClient,
        C::ChunkClient: ChunkServiceProvider,
    {
        let chunk_service = ChunkService::new(self.components.chunk_client().clone());
        let chunk_server = proto::chunk::chunk_server::ChunkServer::new(chunk_service);
        registry.add_service(chunk_server);
    }
}

/// Bootnodes register the node status service only.
impl<T> RegistersGrpcServices for GrpcAdapter<BootnodeComponents<T>>
where
    T: SwarmTopologyState + SwarmTopologyStats + SwarmTopologyPeers + Clone + Send + Sync + 'static,
{
    fn register_grpc_services(&self, registry: &mut GrpcRegistry) {
        self.register_node(registry);
    }
}

/// Client and storer nodes register the node status service and the chunk
/// service.
impl<T, C> RegistersGrpcServices for GrpcAdapter<ClientComponents<T, C>>
where
    T: SwarmTopologyState + SwarmTopologyStats + SwarmTopologyPeers + Clone + Send + Sync + 'static,
    C: ChunkServiceProvider + Send + Sync,
{
    fn register_grpc_services(&self, registry: &mut GrpcRegistry) {
        self.register_node(registry);
        self.register_chunk(registry);

        // TODO: Add storer-specific RPC services (storage, redistribution, etc.)
    }
}
