//! Swarm protocol RPC services for Vertex.
//!
//! This crate provides RPC service implementations for the Swarm protocol.

mod grpc;

pub use grpc::chunk::ChunkService;
pub use grpc::node::NodeService;

// Re-export generated proto types
pub mod proto {
    pub mod node {
        tonic::include_proto!("vertex.swarm.node.v1");
    }

    pub mod chunk {
        tonic::include_proto!("vertex.swarm.chunk.v1");
    }

    /// File descriptor set for gRPC reflection.
    pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("swarm_descriptor");
}

/// Trait for types that can register gRPC services with a tonic router.
///
/// This trait allows protocol providers to add their services to an existing
/// tonic router, enabling composition of services from different sources.
///
/// # Example
///
/// ```ignore
/// use vertex_swarm_rpc::GrpcServiceProvider;
/// use tonic::transport::Server;
///
/// // Start with infrastructure services
/// let router = Server::builder()
///     .add_service(health_server);
///
/// // Add protocol services
/// let router = providers.register_grpc_services(router);
/// ```
pub trait GrpcServiceProvider {
    /// Register this provider's gRPC services with a tonic router.
    ///
    /// Takes an existing router and returns a new router with protocol-specific
    /// services added. This allows chaining multiple providers.
    fn register_grpc_services(
        &self,
        router: tonic::transport::server::Router,
    ) -> tonic::transport::server::Router;
}

// Implementation for RpcProviders
impl<Topo, Chunk> GrpcServiceProvider for vertex_swarm_api::RpcProviders<Topo, Chunk>
where
    Topo: vertex_swarm_api::SwarmTopologyProvider + Clone + 'static,
    Chunk: vertex_swarm_api::SwarmChunkProvider + Clone + 'static,
{
    fn register_grpc_services(
        &self,
        router: tonic::transport::server::Router,
    ) -> tonic::transport::server::Router {
        // Node service for topology
        let node_service = NodeService::new(self.topology.clone());
        let node_server = proto::node::node_server::NodeServer::new(node_service);

        // Chunk service for retrieval
        let chunk_service = ChunkService::new(self.chunk.clone());
        let chunk_server = proto::chunk::chunk_server::ChunkServer::new(chunk_service);

        router.add_service(node_server).add_service(chunk_server)
    }
}
