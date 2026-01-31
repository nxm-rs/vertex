//! RPC service registration for Swarm components.
//!
//! This module provides wrapper types for registering gRPC services with
//! Swarm node components.

use std::sync::Arc;

use vertex_rpc_server::{GrpcRegistry, RegistersGrpcServices};
use vertex_swarm_api::ClientComponents;
use vertex_swarm_identity::Identity;
use vertex_swarm_rpc::{ChunkService, NodeService, proto};
use vertex_swarm_kademlia::KademliaTopology;

use crate::providers::NetworkChunkProvider;
use crate::types::DefaultClientTypes;

/// Wrapper around ClientComponents that implements RegistersGrpcServices.
///
/// This is needed because of Rust's orphan rules - we can't implement an external
/// trait for external types directly. The wrapper allows us to implement the trait.
#[derive(Clone)]
pub struct ClientNodeRpcComponents(pub ClientComponents<DefaultClientTypes>);

impl std::ops::Deref for ClientNodeRpcComponents {
    type Target = ClientComponents<DefaultClientTypes>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<ClientComponents<DefaultClientTypes>> for ClientNodeRpcComponents {
    fn from(components: ClientComponents<DefaultClientTypes>) -> Self {
        Self(components)
    }
}

impl RegistersGrpcServices for ClientNodeRpcComponents {
    fn register_grpc_services(&self, registry: &mut GrpcRegistry) {
        // Create providers from components
        let topology = self.0.topology().clone();
        let client_handle = self.0.client_handle().clone();

        // Create chunk provider with both client handle and topology for peer selection
        let chunk_provider = NetworkChunkProvider::new(client_handle, topology.clone());

        // Create and register node service (uses topology for status info)
        let node_service = NodeService::new(topology);
        let node_server = proto::node::node_server::NodeServer::new(node_service);
        registry.add_service(node_server);

        // Create and register chunk service
        let chunk_service = ChunkService::new(chunk_provider);
        let chunk_server = proto::chunk::chunk_server::ChunkServer::new(chunk_service);
        registry.add_service(chunk_server);

        // Add file descriptors for gRPC reflection
        registry.add_descriptor(proto::FILE_DESCRIPTOR_SET);
    }
}

/// RPC components for bootnodes.
///
/// Bootnodes only expose node status (topology info), no chunk service.
pub struct BootnodeRpcComponents {
    /// Node identity.
    pub identity: Arc<Identity>,
    /// Kademlia topology.
    pub topology: Arc<KademliaTopology<Arc<Identity>>>,
}

impl RegistersGrpcServices for BootnodeRpcComponents {
    fn register_grpc_services(&self, registry: &mut GrpcRegistry) {
        // Create and register node service (uses topology for status info)
        let node_service = NodeService::new(self.topology.clone());
        let node_server = proto::node::node_server::NodeServer::new(node_service);
        registry.add_service(node_server);

        // Add file descriptors for gRPC reflection
        registry.add_descriptor(proto::FILE_DESCRIPTOR_SET);
    }
}
