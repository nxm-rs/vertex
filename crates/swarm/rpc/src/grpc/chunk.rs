//! Chunk service implementation for Swarm chunk retrieval and upload.

use hex::FromHex;
use tonic::{Request, Response, Status};
use vertex_swarm_api::{
    ChunkAddress, PushReceipt, Stamp, StampedChunk, SwarmChunkProvider, SwarmChunkSender,
};

use crate::proto::chunk::{
    HasChunkRequest, HasChunkResponse, RetrieveChunkRequest, RetrieveChunkResponse,
    UploadChunkRequest, UploadChunkResponse, chunk_server::Chunk,
};

/// Chunk service implementation.
///
/// Provides gRPC endpoints for retrieving and uploading chunks on the Swarm
/// network.
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

/// Build a [`StampedChunk`] from an upload request payload.
///
/// The proto carries raw bytes at this boundary; convert them to typed values
/// immediately. The chunk is reconstructed from `data` and verified against
/// `address`: a lying address makes reconstruction fail, so the address is
/// self-validating against the payload. The stamp is parsed from its wire bytes
/// and paired with the chunk.
#[allow(clippy::result_large_err)]
fn parse_stamped_chunk(req: &UploadChunkRequest) -> Result<StampedChunk, Status> {
    let address = parse_chunk_address(&req.address)?;

    let stamp = Stamp::try_from_slice(&req.stamp)
        .map_err(|e| Status::invalid_argument(format!("Invalid postage stamp: {}", e)))?;

    StampedChunk::reconstruct(address, req.data.clone().into(), stamp).map_err(|e| {
        Status::invalid_argument(format!("Chunk does not match the supplied address: {}", e))
    })
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
    /// The chunk and its postage stamp travel together as a [`StampedChunk`] and
    /// are forwarded to the responsible storer via PushSync. The response carries
    /// the full [`PushReceipt`] so the caller receives the storer's proof of
    /// acceptance.
    async fn upload_chunk(
        &self,
        request: Request<UploadChunkRequest>,
    ) -> Result<Response<UploadChunkResponse>, Status> {
        let req = request.into_inner();
        let stamped = parse_stamped_chunk(&req)?;

        // Pre-stamped uploads are trusted by default; opt in to validation.
        let receipt = if req.validate {
            self.provider.send_chunk(stamped).await
        } else {
            self.provider.send_chunk_unchecked(stamped).await
        }
        .map_err(|e| Status::internal(format!("Chunk upload failed: {}", e)))?;

        let PushReceipt {
            signer,
            signature,
            nonce,
            storage_radius,
        } = receipt;

        Ok(Response::new(UploadChunkResponse {
            // The `storer` wire field carries the recovered receipt signer: the
            // node that actually took custody, not whichever peer relayed it.
            storer: signer.to_string(),
            signature: signature.as_bytes().to_vec(),
            nonce: nonce.as_slice().to_vec(),
            storage_radius: u32::from(storage_radius.get()),
        }))
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use vertex_swarm_api::{AnyChunk, Chunk as _, ContentChunk};

    use super::*;
    use crate::proto::chunk::ChunkType;

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
    fn parse_stamped_content_chunk_roundtrips() {
        let (wire, address) = content_wire(b"reconstruct me");

        let req = content_request(wire, &address);
        let stamped = parse_stamped_chunk(&req).expect("valid stamped chunk");

        assert_eq!(stamped.address(), &address);
        assert!(matches!(stamped.chunk(), AnyChunk::Content(_)));
    }

    #[test]
    fn parse_rejects_malformed_address() {
        let (wire, _) = content_wire(b"any");
        let mut req = content_request(wire, &ChunkAddress::default());
        req.address = "not-hex".to_string();

        let err = parse_stamped_chunk(&req).expect_err("malformed address must fail");
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn parse_rejects_address_mismatch() {
        let (wire, _) = content_wire(b"payload");
        // Address that does not match the BMT hash of the payload.
        let wrong = ChunkAddress::new([0xab; 32]);

        let req = content_request(wire, &wrong);
        let err = parse_stamped_chunk(&req).expect_err("address mismatch must fail");
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn parse_rejects_malformed_stamp() {
        let (wire, address) = content_wire(b"payload");
        let mut req = content_request(wire, &address);
        // A stamp shorter than the fixed wire size cannot parse.
        req.stamp = vec![0u8; 10];

        let err = parse_stamped_chunk(&req).expect_err("malformed stamp must fail");
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }
}
