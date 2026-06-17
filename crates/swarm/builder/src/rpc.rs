//! The generic [`NodeProviders`] container and the single gRPC registration
//! path.
//!
//! One [`NodeProviders<C>`] wraps a role components type `C`; one
//! [`RegistersGrpcServices`] impl drives every role by delegating to `C`'s
//! [`RegisterSwarmServices`] impl. The node status service is always registered;
//! the chunk service is registered only by components that carry a chunk client
//! (`C: HasChunkClient`). Role-specific components types ([`TopologyComponents`],
//! [`ChunkComponents`]) decide which capabilities are registered.

use vertex_rpc_server::{GrpcRegistry, RegistersGrpcServices};
use vertex_swarm_api::{HasChunkClient, HasTopology, SwarmIdentity, SwarmTopology};
use vertex_swarm_rpc::{ChunkService, ChunkServiceProvider, NodeService, proto};
use vertex_swarm_topology::TopologyHandle;

/// Register the node status service and the shared reflection descriptor.
fn register_node_service<T: SwarmTopology + Clone + 'static>(
    registry: &mut GrpcRegistry,
    topology: &T,
) {
    let node_service = NodeService::new(topology.clone());
    let node_server = proto::node::node_server::NodeServer::new(node_service);
    registry.add_service(node_server);

    registry.add_descriptor(proto::FILE_DESCRIPTOR_SET);
}

/// Register the chunk upload/download service that backs client and storer
/// nodes.
fn register_chunk_service<C: ChunkServiceProvider>(registry: &mut GrpcRegistry, chunks: &C) {
    let chunk_service = ChunkService::new(chunks.clone());
    let chunk_server = proto::chunk::chunk_server::ChunkServer::new(chunk_service);
    registry.add_service(chunk_server);
}

/// One generic providers container over a role components type `C`.
///
/// Replaces the per-role `*RpcProviders` containers: the gRPC surface is driven
/// by the capabilities `C` exposes. Capability access ([`HasTopology`],
/// [`HasChunkClient`]) delegates to `C`.
#[derive(Debug, Clone)]
pub struct NodeProviders<C> {
    components: C,
}

impl<C> NodeProviders<C> {
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

impl<C: HasTopology> HasTopology for NodeProviders<C> {
    type Topology = C::Topology;

    fn topology(&self) -> &Self::Topology {
        self.components.topology()
    }
}

impl<C: HasChunkClient> HasChunkClient for NodeProviders<C> {
    type ChunkClient = C::ChunkClient;

    fn chunk_client(&self) -> &Self::ChunkClient {
        self.components.chunk_client()
    }
}

/// Per-components registration of the Swarm gRPC surface.
///
/// Each concrete components type registers exactly the services its role
/// exposes, so [`NodeProviders`] needs only one delegating
/// [`RegistersGrpcServices`] impl. This sidesteps the overlapping-impl problem
/// of gating chunk registration on `HasChunkClient` at the `NodeProviders<C>`
/// level.
pub trait RegisterSwarmServices {
    /// Register this role's gRPC services with the registry.
    fn register_swarm_services(&self, registry: &mut GrpcRegistry);
}

/// Components for bootnodes: topology only.
pub struct TopologyComponents<I: SwarmIdentity> {
    topology: TopologyHandle<I>,
}

impl<I: SwarmIdentity> TopologyComponents<I> {
    /// Create from the topology handle of a built bootnode.
    pub fn new(topology: TopologyHandle<I>) -> Self {
        Self { topology }
    }
}

impl<I: SwarmIdentity> HasTopology for TopologyComponents<I> {
    type Topology = TopologyHandle<I>;

    fn topology(&self) -> &TopologyHandle<I> {
        &self.topology
    }
}

impl<I: SwarmIdentity> RegisterSwarmServices for TopologyComponents<I> {
    fn register_swarm_services(&self, registry: &mut GrpcRegistry) {
        register_node_service(registry, &self.topology);
    }
}

/// Components for client and storer nodes: topology + chunk client.
pub struct ChunkComponents<I: SwarmIdentity, C> {
    topology: TopologyHandle<I>,
    chunks: C,
}

impl<I: SwarmIdentity, C> ChunkComponents<I, C> {
    /// Create from the topology handle and chunk provider of a built node.
    pub fn new(topology: TopologyHandle<I>, chunks: C) -> Self {
        Self { topology, chunks }
    }

    /// Access the chunk provider that backs uploads and downloads.
    ///
    /// Embedders that drive a node directly (FFI, gRPC, or an example) borrow
    /// this to call the chunk client without going through the gRPC surface.
    pub fn chunks(&self) -> &C {
        &self.chunks
    }
}

impl<I: SwarmIdentity, C: Send + Sync> HasTopology for ChunkComponents<I, C> {
    type Topology = TopologyHandle<I>;

    fn topology(&self) -> &TopologyHandle<I> {
        &self.topology
    }
}

impl<I: SwarmIdentity, C: Send + Sync> HasChunkClient for ChunkComponents<I, C> {
    type ChunkClient = C;

    fn chunk_client(&self) -> &C {
        &self.chunks
    }
}

impl<I: SwarmIdentity, C: ChunkServiceProvider> RegisterSwarmServices for ChunkComponents<I, C> {
    fn register_swarm_services(&self, registry: &mut GrpcRegistry) {
        register_node_service(registry, &self.topology);
        register_chunk_service(registry, &self.chunks);

        // TODO: Add storer-specific RPC services (storage, redistribution, etc.)
    }
}

/// Single registration path for every node role: delegate to the wrapped
/// components, which register exactly the services their role exposes.
impl<C: RegisterSwarmServices + Send + Sync> RegistersGrpcServices for NodeProviders<C> {
    fn register_grpc_services(&self, registry: &mut GrpcRegistry) {
        self.components.register_swarm_services(registry);
    }
}
