//! RPC providers for Swarm nodes.
//!
//! Each node type has its own provider struct that implements `RegistersGrpcServices`.

use std::sync::Arc;

use vertex_rpc_server::{GrpcRegistry, RegistersGrpcServices};
use vertex_swarm_api::SwarmChunkProvider;
use vertex_swarm_identity::Identity;
use vertex_swarm_kademlia::KademliaTopology;
use vertex_swarm_rpc::{ChunkService, NodeService, proto};

/// RPC providers for client nodes.
///
/// Provides both node status (topology) and chunk retrieval.
pub struct ClientRpcProviders<C> {
    /// Topology for node status RPC.
    pub topology: Arc<KademliaTopology<Arc<Identity>>>,
    /// Chunk provider for chunk retrieval RPC.
    pub chunks: C,
}

impl<C> ClientRpcProviders<C> {
    /// Create new client RPC providers.
    pub fn new(topology: Arc<KademliaTopology<Arc<Identity>>>, chunks: C) -> Self {
        Self { topology, chunks }
    }
}

impl<C: SwarmChunkProvider + Clone> RegistersGrpcServices for ClientRpcProviders<C> {
    fn register_grpc_services(&self, registry: &mut GrpcRegistry) {
        // Register node service (topology status)
        let node_service = NodeService::new(self.topology.clone());
        let node_server = proto::node::node_server::NodeServer::new(node_service);
        registry.add_service(node_server);

        // Register chunk service
        let chunk_service = ChunkService::new(self.chunks.clone());
        let chunk_server = proto::chunk::chunk_server::ChunkServer::new(chunk_service);
        registry.add_service(chunk_server);

        registry.add_descriptor(proto::FILE_DESCRIPTOR_SET);
    }
}

/// RPC providers for bootnodes.
///
/// Only provides node status (topology), no chunk service.
pub struct BootnodeRpcProviders {
    /// Topology for node status RPC.
    pub topology: Arc<KademliaTopology<Arc<Identity>>>,
}

impl BootnodeRpcProviders {
    /// Create new bootnode RPC providers.
    pub fn new(topology: Arc<KademliaTopology<Arc<Identity>>>) -> Self {
        Self { topology }
    }
}

impl RegistersGrpcServices for BootnodeRpcProviders {
    fn register_grpc_services(&self, registry: &mut GrpcRegistry) {
        // Register node service (topology status only)
        let node_service = NodeService::new(self.topology.clone());
        let node_server = proto::node::node_server::NodeServer::new(node_service);
        registry.add_service(node_server);

        registry.add_descriptor(proto::FILE_DESCRIPTOR_SET);
    }
}
