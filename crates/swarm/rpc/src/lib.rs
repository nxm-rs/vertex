//! Swarm protocol RPC services for Vertex.
//!
//! This crate provides RPC service implementations for the Swarm protocol.

mod grpc;

pub use grpc::chunk::{ChunkService, StampValidation};
pub use grpc::node::NodeService;

/// A chunk provider usable by the chunk gRPC service.
///
/// Absorbed into [`vertex_swarm_stream::ChunkClient`]: kept as a re-export so
/// existing bounds (`P: ChunkServiceProvider`) keep compiling against the one
/// capability alias. The chunk service drives off [`ChunkClientExt`] and the
/// free helpers, so this no longer carries its own definition.
///
/// [`ChunkClientExt`]: vertex_swarm_stream::ChunkClientExt
pub use vertex_swarm_stream::ChunkClient as ChunkServiceProvider;

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
