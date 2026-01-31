//! Chunk service implementation for Swarm chunk retrieval.

use tonic::{Request, Response, Status};
use vertex_swarm_api::SwarmChunkProvider;

use crate::proto::chunk::{
    HasChunkRequest, HasChunkResponse, RetrieveChunkRequest, RetrieveChunkResponse,
    chunk_server::Chunk,
};

/// Chunk service implementation.
///
/// Provides gRPC endpoints for retrieving chunks from the Swarm network.
pub struct ChunkService<P> {
    provider: P,
}

impl<P> ChunkService<P> {
    /// Create a new chunk service with the given provider.
    pub fn new(provider: P) -> Self {
        Self { provider }
    }
}

#[tonic::async_trait]
impl<P: SwarmChunkProvider> Chunk for ChunkService<P> {
    async fn retrieve_chunk(
        &self,
        request: Request<RetrieveChunkRequest>,
    ) -> Result<Response<RetrieveChunkResponse>, Status> {
        let req = request.into_inner();

        // Validate address format (should be 64 hex chars)
        if req.address.len() != 64 || !req.address.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Status::invalid_argument(format!(
                "Invalid chunk address: expected 64 hex characters, got '{}'",
                req.address
            )));
        }

        // Retrieve the chunk via the provider
        match self.provider.retrieve_chunk(&req.address).await {
            Ok(result) => Ok(Response::new(RetrieveChunkResponse {
                data: result.data.to_vec(),
                stamp: result.stamp.to_vec(),
                served_by: result.served_by,
            })),
            Err(e) => Err(Status::internal(format!("Chunk retrieval failed: {}", e))),
        }
    }

    async fn has_chunk(
        &self,
        request: Request<HasChunkRequest>,
    ) -> Result<Response<HasChunkResponse>, Status> {
        let req = request.into_inner();

        // Validate address format
        if req.address.len() != 64 || !req.address.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Status::invalid_argument(format!(
                "Invalid chunk address: expected 64 hex characters, got '{}'",
                req.address
            )));
        }

        let exists = self.provider.has_chunk(&req.address);
        Ok(Response::new(HasChunkResponse { exists }))
    }
}
