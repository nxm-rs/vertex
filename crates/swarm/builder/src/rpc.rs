//! The generic [`NodeProviders`] container and the single gRPC registration
//! path.
//!
//! One [`NodeProviders<C>`] wraps an api component container `C`; one
//! [`RegistersGrpcServices`] impl drives every role by delegating to `C`'s
//! [`RegisterSwarmServices`] impl. The node status service is always registered;
//! the chunk service is registered only by components that carry a chunk client
//! ([`ClientComponents`]). The api component containers
//! ([`BootnodeComponents`](vertex_swarm_api::BootnodeComponents),
//! [`ClientComponents`]) decide which capabilities are registered through their
//! [`RegisterSwarmServices`] impls.

use vertex_rpc_server::{GrpcRegistry, RegistersGrpcServices};
use vertex_swarm_api::{
    BootnodeComponents, ClientComponents, HasChunkClient, HasTopology, SwarmTopology,
};
use vertex_swarm_rpc::{ChunkService, ChunkServiceProvider, NodeService, proto};

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

/// One generic providers container over an api component container `C`.
///
/// The gRPC surface is driven by the capabilities `C` exposes. Capability access
/// ([`HasTopology`], [`HasChunkClient`]) delegates to `C`.
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
/// Each api component container registers exactly the services its role exposes,
/// so [`NodeProviders`] needs only one delegating [`RegistersGrpcServices`]
/// impl. This sidesteps the overlapping-impl problem of gating chunk
/// registration on `HasChunkClient` at the `NodeProviders<C>` level.
pub trait RegisterSwarmServices {
    /// Register this role's gRPC services with the registry.
    fn register_swarm_services(&self, registry: &mut GrpcRegistry);
}

/// Bootnodes register the node status service only.
impl<T: SwarmTopology + Clone + 'static> RegisterSwarmServices for BootnodeComponents<T> {
    fn register_swarm_services(&self, registry: &mut GrpcRegistry) {
        register_node_service(registry, self.topology());
    }
}

/// Client and storer nodes register the node status service and the chunk
/// service.
impl<T: SwarmTopology + Clone + 'static, C: ChunkServiceProvider> RegisterSwarmServices
    for ClientComponents<T, C>
{
    fn register_swarm_services(&self, registry: &mut GrpcRegistry) {
        register_node_service(registry, self.topology());
        register_chunk_service(registry, self.chunk_client());

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
