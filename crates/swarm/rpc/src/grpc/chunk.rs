//! Chunk service implementation for Swarm chunk retrieval.

use hex::FromHex;
use tonic::{Request, Response, Status};
use vertex_swarm_api::{ChunkAddress, SwarmChunkProvider};

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

/// Parse a hex string into a ChunkAddress.
fn parse_chunk_address(hex: &str) -> Result<ChunkAddress, Status> {
    let bytes = <[u8; 32]>::from_hex(hex).map_err(|_| {
        Status::invalid_argument(format!(
            "Invalid chunk address: expected 64 hex characters, got '{}'",
            hex
        ))
    })?;
    Ok(ChunkAddress::new(bytes))
}

#[tonic::async_trait]
impl<P: SwarmChunkProvider> Chunk for ChunkService<P> {
    async fn retrieve_chunk(
        &self,
        request: Request<RetrieveChunkRequest>,
    ) -> Result<Response<RetrieveChunkResponse>, Status> {
        let req = request.into_inner();
        let address = parse_chunk_address(&req.address)?;

        // Retrieve the chunk via the provider
        match self.provider.retrieve_chunk(&address).await {
            Ok(result) => Ok(Response::new(RetrieveChunkResponse {
                data: result.data.to_vec(),
                stamp: result.stamp.to_vec(),
                served_by: result.served_by.to_string(),
            })),
            Err(e) => Err(Status::internal(format!("Chunk retrieval failed: {}", e))),
        }
    }

    async fn has_chunk(
        &self,
        request: Request<HasChunkRequest>,
    ) -> Result<Response<HasChunkResponse>, Status> {
        let req = request.into_inner();
        let address = parse_chunk_address(&req.address)?;

        let exists = self.provider.has_chunk(&address);
        Ok(Response::new(HasChunkResponse { exists }))
    }
}
