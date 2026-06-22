//! gRPC adapter over an api component container.
//!
//! [`GrpcAdapter<C>`] registers exactly the services `C`'s capabilities expose:
//! the node status service is gated on [`HasTopology`], the chunk service on
//! [`HasChunkClient`]. Registration uses per-shape [`RegistersGrpcServices`]
//! impls (one per concrete container) to avoid overlapping blanket impls for the
//! optional chunk capability.

use vertex_rpc_server::{GrpcRegistry, RegistersGrpcServices};
use vertex_swarm_api::{
    BinCursorStore, BootnodeComponents, ClientComponents, HasChunkClient, HasReserve, HasStore,
    HasTopology, StorerComponents, SwarmTopologyPeers, SwarmTopologyState, SwarmTopologyStats,
};
use vertex_swarm_stream::ChunkClient;

use crate::{ChunkService, NodeService, ReserveService, proto};

/// gRPC adapter over an api component container `C`; capability accessors
/// delegate to `C`.
#[derive(Debug, Clone)]
pub struct GrpcAdapter<C> {
    components: C,
}

impl<C> GrpcAdapter<C> {
    pub fn new(components: C) -> Self {
        Self { components }
    }

    pub fn components(&self) -> &C {
        &self.components
    }

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

impl<C: HasStore> HasStore for GrpcAdapter<C> {
    type Store = C::Store;

    fn store(&self) -> &Self::Store {
        self.components.store()
    }
}

impl<C: HasReserve> HasReserve for GrpcAdapter<C> {
    type Reserve = C::Reserve;

    fn reserve(&self) -> &Self::Reserve {
        self.components.reserve()
    }
}

impl<C> GrpcAdapter<C> {
    /// Register the node status service and the shared reflection descriptor.
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
    pub fn register_chunk(&self, registry: &mut GrpcRegistry)
    where
        C: HasChunkClient,
        C::ChunkClient: ChunkClient,
    {
        let chunk_service = ChunkService::new(self.components.chunk_client().clone());
        let chunk_server = proto::chunk::chunk_server::ChunkServer::new(chunk_service);
        registry.add_service(chunk_server);
    }

    /// Register the storer reserve service.
    pub fn register_reserve(&self, registry: &mut GrpcRegistry)
    where
        C: HasReserve,
        C::Reserve: BinCursorStore + Clone + 'static,
    {
        let reserve_service = ReserveService::new(self.components.reserve().clone());
        let reserve_server = proto::reserve::reserve_server::ReserveServer::new(reserve_service);
        registry.add_service(reserve_server);
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

/// Client nodes register the node status service and the chunk service.
impl<T, C> RegistersGrpcServices for GrpcAdapter<ClientComponents<T, C>>
where
    T: SwarmTopologyState + SwarmTopologyStats + SwarmTopologyPeers + Clone + Send + Sync + 'static,
    C: ChunkClient + Send + Sync,
{
    fn register_grpc_services(&self, registry: &mut GrpcRegistry) {
        self.register_node(registry);
        self.register_chunk(registry);
    }
}

/// Storer nodes register the node status service, the chunk service, and the
/// reserve service over the `R` reserve axis.
impl<T, C, S, R> RegistersGrpcServices for GrpcAdapter<StorerComponents<T, C, S, R>>
where
    T: SwarmTopologyState + SwarmTopologyStats + SwarmTopologyPeers + Clone + Send + Sync + 'static,
    C: ChunkClient + Send + Sync,
    S: Send + Sync,
    R: BinCursorStore + Clone + 'static,
{
    fn register_grpc_services(&self, registry: &mut GrpcRegistry) {
        self.register_node(registry);
        self.register_chunk(registry);
        self.register_reserve(registry);
    }
}
