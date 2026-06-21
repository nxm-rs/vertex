//! Swarm protocol RPC services for Vertex.
//!
//! This crate provides RPC service implementations for the Swarm protocol.

mod adapter;
mod grpc;

pub use adapter::GrpcAdapter;
pub use grpc::chunk::{ChunkService, StampValidation};
pub use grpc::node::NodeService;
pub use grpc::reserve::ReserveService;

pub mod proto {
    pub mod node {
        tonic::include_proto!("vertex.swarm.node.v1");
    }

    pub mod chunk {
        tonic::include_proto!("vertex.swarm.chunk.v1");
    }

    pub mod reserve {
        tonic::include_proto!("vertex.swarm.reserve.v1");
    }

    /// File descriptor set for gRPC reflection.
    pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("swarm_descriptor");
}
