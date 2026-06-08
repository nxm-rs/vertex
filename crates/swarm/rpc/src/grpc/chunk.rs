//! Chunk service implementation for Swarm chunk retrieval.

use hex::FromHex;
use tonic::{Request, Response, Status};
use vertex_swarm_api::{
    AnyChunk, ChunkAddress, ContentChunk, SingleOwnerChunk, SwarmChunkProvider, SwarmChunkSender,
};

use crate::proto::chunk::{
    ChunkType, HasChunkRequest, HasChunkResponse, RetrieveChunkRequest, RetrieveChunkResponse,
    UploadChunkRequest, UploadChunkResponse, chunk_server::Chunk,
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
#[allow(clippy::result_large_err)]
fn parse_chunk_address(hex: &str) -> Result<ChunkAddress, Status> {
    let bytes = <[u8; 32]>::from_hex(hex).map_err(|_| {
        Status::invalid_argument(format!(
            "Invalid chunk address: expected 64 hex characters, got '{}'",
            hex
        ))
    })?;
    Ok(ChunkAddress::new(bytes))
}

/// Reconstruct an `AnyChunk` from an upload request payload.
///
/// The `data` field is interpreted according to `chunk_type`: a content chunk
/// carries the BMT body, a single-owner chunk carries the full wire encoding.
/// The reconstructed chunk is verified against the supplied address.
#[allow(clippy::result_large_err)]
fn reconstruct_chunk(req: &UploadChunkRequest) -> Result<AnyChunk, Status> {
    let address = parse_chunk_address(&req.address)?;

    let chunk_type = ChunkType::try_from(req.chunk_type)
        .map_err(|_| Status::invalid_argument(format!("Unknown chunk type: {}", req.chunk_type)))?;

    let chunk = match chunk_type {
        ChunkType::Content => {
            // Parse the full BMT body (span + data) from the wire encoding, the
            // same shape `RetrieveChunk` returns, so a retrieved content chunk
            // round-trips back into an upload. The BMT hash is recomputed from
            // the body, letting the verify step below reject an address that
            // lies about the payload.
            let content = ContentChunk::try_from(req.data.as_slice())
                .map_err(|e| Status::invalid_argument(format!("Invalid content chunk: {}", e)))?;
            AnyChunk::Content(content)
        }
        ChunkType::SingleOwner => {
            let soc = SingleOwnerChunk::try_from(req.data.as_slice()).map_err(|e| {
                Status::invalid_argument(format!("Invalid single-owner chunk: {}", e))
            })?;
            AnyChunk::SingleOwner(soc)
        }
    };

    chunk.verify(&address).map_err(|e| {
        Status::invalid_argument(format!("Chunk address does not match payload: {}", e))
    })?;

    Ok(chunk)
}

#[tonic::async_trait]
impl<P: SwarmChunkProvider + SwarmChunkSender> Chunk for ChunkService<P> {
    async fn retrieve_chunk(
        &self,
        request: Request<RetrieveChunkRequest>,
    ) -> Result<Response<RetrieveChunkResponse>, Status> {
        let req = request.into_inner();
        let address = parse_chunk_address(&req.address)?;

        // Retrieve the chunk via the provider
        match self.provider.retrieve_chunk(&address).await {
            Ok(result) => {
                let served_by = result.served_by.to_string();
                let (chunk, stamp) = result.chunk.into_parts();
                Ok(Response::new(RetrieveChunkResponse {
                    data: chunk.into_bytes().to_vec(),
                    stamp: stamp.to_bytes().to_vec(),
                    served_by,
                }))
            }
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

    /// Upload a pre-stamped chunk to the network.
    ///
    /// The request carries the postage stamp in `stamp`; it is threaded to the
    /// storer once `SwarmChunkSender` accepts the stamp alongside the chunk.
    async fn upload_chunk(
        &self,
        request: Request<UploadChunkRequest>,
    ) -> Result<Response<UploadChunkResponse>, Status> {
        let req = request.into_inner();
        let chunk = reconstruct_chunk(&req)?;

        // Pre-stamped uploads are trusted by default; opt in to validation.
        let receipt = if req.validate {
            self.provider.send_chunk(chunk).await
        } else {
            self.provider.send_chunk_unchecked(chunk).await
        }
        .map_err(|e| Status::internal(format!("Chunk upload failed: {}", e)))?;

        Ok(Response::new(UploadChunkResponse {
            storer: receipt.storer.to_string(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use vertex_swarm_api::Chunk as _;

    use super::*;

    /// Build a content chunk from raw data and return its wire encoding (span +
    /// data) alongside its address, matching what the upload handler expects.
    fn content_wire(data: &[u8]) -> (Vec<u8>, ChunkAddress) {
        let chunk: ContentChunk = ContentChunk::new(data.to_vec()).expect("valid content chunk");
        let address = *chunk.address();
        (Bytes::from(chunk).to_vec(), address)
    }

    fn content_request(data: Vec<u8>, address: &ChunkAddress) -> UploadChunkRequest {
        UploadChunkRequest {
            data,
            stamp: vec![0u8; 113],
            address: hex::encode(address.as_bytes()),
            chunk_type: ChunkType::Content as i32,
            validate: false,
        }
    }

    #[test]
    fn reconstruct_content_chunk_roundtrips() {
        let (wire, address) = content_wire(b"reconstruct me");

        let req = content_request(wire, &address);
        let chunk = reconstruct_chunk(&req).expect("valid content chunk");

        assert_eq!(chunk.address(), &address);
        assert!(matches!(chunk, AnyChunk::Content(_)));
    }

    #[test]
    fn reconstruct_rejects_malformed_address() {
        let (wire, _) = content_wire(b"any");
        let mut req = content_request(wire, &ChunkAddress::default());
        req.address = "not-hex".to_string();

        let err = reconstruct_chunk(&req).expect_err("malformed address must fail");
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn reconstruct_rejects_address_mismatch() {
        let (wire, _) = content_wire(b"payload");
        // Address that does not match the BMT hash of the payload.
        let wrong = ChunkAddress::new([0xab; 32]);

        let req = content_request(wire, &wrong);
        let err = reconstruct_chunk(&req).expect_err("address mismatch must fail");
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn reconstruct_rejects_unknown_chunk_type() {
        let (wire, address) = content_wire(b"payload");
        let mut req = content_request(wire, &address);
        req.chunk_type = 99;

        let err = reconstruct_chunk(&req).expect_err("unknown chunk type must fail");
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }
}
