//! Chunk service: retrieval, upload, and existence checks over Swarm.

use std::pin::Pin;

use futures::StreamExt;
use tonic::{Request, Response, Status, Streaming};
use vertex_swarm_api::{ChunkAddress, PushReceipt, Stamp, StampedChunk, SwarmError};
use vertex_swarm_stream::{
    NATIVE_DOWNLOAD_CONCURRENCY, StreamConfig, VerifiedChunk, retrieve_verified,
};

use crate::ChunkServiceProvider;
use crate::proto::chunk::{
    ChunkError, HasChunkRequest, HasChunkResponse, RetrieveChunkRequest, RetrieveChunkResponse,
    RetrievedChunk, UploadChunkRequest, UploadChunkResponse, UploadReceipt, chunk_server::Chunk,
    retrieve_chunk_response, upload_chunk_response,
};

/// gRPC chunk retrieval and upload service.
pub struct ChunkService<P> {
    provider: P,
}

impl<P> ChunkService<P> {
    /// Create a new chunk service with the given provider.
    pub fn new(provider: P) -> Self {
        Self { provider }
    }
}

/// Parse a 32-byte chunk address from raw bytes.
#[allow(clippy::result_large_err)]
fn parse_chunk_address(bytes: &[u8]) -> Result<ChunkAddress, Status> {
    let arr = <[u8; 32]>::try_from(bytes).map_err(|_| {
        Status::invalid_argument(format!(
            "invalid chunk address: expected 32 bytes, got {}",
            bytes.len()
        ))
    })?;
    Ok(ChunkAddress::new(arr))
}

/// Build a [`StampedChunk`] from an upload request; the address self-validates
/// against the payload via reconstruction.
#[allow(clippy::result_large_err)]
fn parse_stamped_chunk(req: &UploadChunkRequest) -> Result<StampedChunk, Status> {
    let address = parse_chunk_address(&req.address)?;

    let stamp = Stamp::try_from_slice(&req.stamp)
        .map_err(|e| Status::invalid_argument(format!("invalid postage stamp: {e}")))?;

    StampedChunk::reconstruct(address, req.data.clone().into(), stamp).map_err(|e| {
        Status::invalid_argument(format!("chunk does not match the supplied address: {e}"))
    })
}

/// Map a unary retrieval failure to a gRPC status: absence becomes `not_found`,
/// all else `internal`.
#[allow(clippy::result_large_err)]
fn retrieval_status(error: &SwarmError) -> Status {
    match error {
        SwarmError::ChunkNotFound { .. } | SwarmError::NoStorer { .. } => {
            Status::not_found(format!("chunk not found: {error}"))
        }
        other => Status::internal(format!("chunk retrieval failed: {other}")),
    }
}

/// A successful retrieval response carrying the chunk and its address.
fn retrieved_response(
    address: ChunkAddress,
    data: Vec<u8>,
    stamp: Vec<u8>,
    served_by: Vec<u8>,
) -> RetrieveChunkResponse {
    RetrieveChunkResponse {
        result: Some(retrieve_chunk_response::Result::Chunk(RetrievedChunk {
            address: address.as_bytes().to_vec(),
            data,
            stamp,
            served_by,
        })),
    }
}

/// A per-address retrieval failure response. `address` is the raw requested
/// bytes, echoed for correlation (even when they failed to parse).
fn retrieve_error(address: Vec<u8>, message: String) -> RetrieveChunkResponse {
    RetrieveChunkResponse {
        result: Some(retrieve_chunk_response::Result::Error(ChunkError {
            address,
            message,
        })),
    }
}

/// Map a verified download item onto the wire response (`served_by` unknown on
/// the streaming path, so emitted empty).
fn verified_response(address: ChunkAddress, verified: VerifiedChunk) -> RetrieveChunkResponse {
    let (chunk, stamp) = verified.into_parts();
    retrieved_response(
        address,
        chunk.into_bytes().to_vec(),
        stamp.map(|s| s.to_bytes().to_vec()).unwrap_or_default(),
        Vec::new(),
    )
}

/// A successful upload receipt response carrying the chunk address.
fn receipt_response(address: ChunkAddress, receipt: PushReceipt) -> UploadChunkResponse {
    let PushReceipt {
        storer,
        signature,
        nonce,
        storage_radius,
    } = receipt;

    UploadChunkResponse {
        result: Some(upload_chunk_response::Result::Receipt(UploadReceipt {
            address: address.as_bytes().to_vec(),
            storer: storer.as_bytes().to_vec(),
            signature: signature.as_bytes().to_vec(),
            nonce: nonce.as_slice().to_vec(),
            storage_radius: u32::from(storage_radius.get()),
        })),
    }
}

/// A per-chunk upload failure response. `address` is the raw request bytes,
/// echoed for correlation (even when they failed to parse).
fn upload_error(address: Vec<u8>, message: String) -> UploadChunkResponse {
    UploadChunkResponse {
        result: Some(upload_chunk_response::Result::Error(ChunkError {
            address,
            message,
        })),
    }
}

/// Boxed item-result stream returned by the streaming RPCs.
type ResponseStream<T> = Pin<Box<dyn tokio_stream::Stream<Item = Result<T, Status>> + Send>>;

#[tonic::async_trait]
impl<P: ChunkServiceProvider> Chunk for ChunkService<P> {
    async fn retrieve_chunk(
        &self,
        request: Request<RetrieveChunkRequest>,
    ) -> Result<Response<RetrieveChunkResponse>, Status> {
        let req = request.into_inner();
        let address = parse_chunk_address(&req.address)?;

        match self.provider.retrieve_chunk(&address).await {
            Ok(result) => {
                let served_by = result.served_by.as_bytes().to_vec();
                let stamp = result
                    .stamp
                    .map(|s| s.to_bytes().to_vec())
                    .unwrap_or_default();
                Ok(Response::new(retrieved_response(
                    address,
                    result.chunk.into_bytes().to_vec(),
                    stamp,
                    served_by,
                )))
            }
            Err(e) => Err(retrieval_status(&e)),
        }
    }

    async fn has_chunk(
        &self,
        request: Request<HasChunkRequest>,
    ) -> Result<Response<HasChunkResponse>, Status> {
        let req = request.into_inner();
        let address = parse_chunk_address(&req.address)?;
        Ok(Response::new(HasChunkResponse {
            address: address.as_bytes().to_vec(),
            exists: self.provider.has_chunk(&address),
        }))
    }

    /// Upload a pre-stamped chunk and return the storer's push receipt.
    async fn upload_chunk(
        &self,
        request: Request<UploadChunkRequest>,
    ) -> Result<Response<UploadChunkResponse>, Status> {
        let req = request.into_inner();
        let address = parse_chunk_address(&req.address)?;
        let stamped = parse_stamped_chunk(&req)?;

        // Pre-stamped uploads are trusted by default; opt in to validation.
        let receipt = if req.validate {
            self.provider.send_chunk(stamped).await
        } else {
            self.provider.send_chunk_unchecked(stamped).await
        }
        .map_err(|e| Status::internal(format!("chunk upload failed: {e}")))?;

        Ok(Response::new(receipt_response(address, receipt)))
    }

    type UploadChunksStream = ResponseStream<UploadChunkResponse>;

    /// Upload a stream of pre-stamped chunks; receipts return in completion
    /// order, each carrying its address. A per-chunk failure is one error item,
    /// not a teardown.
    ///
    /// Pushes are driven directly off the inbound stream via `buffer_unordered`,
    /// which pulls a new request only as a push slot frees, so server memory is
    /// bounded by the concurrency, not by the (untrusted) request count.
    async fn upload_chunks(
        &self,
        request: Request<Streaming<UploadChunkRequest>>,
    ) -> Result<Response<Self::UploadChunksStream>, Status> {
        let inbound = request.into_inner();
        let provider = self.provider.clone();

        let out = inbound
            .map(move |item| {
                let provider = provider.clone();
                async move {
                    let req = item.map_err(|status| {
                        Status::internal(format!("inbound stream error: {status}"))
                    })?;
                    let validate = req.validate;
                    Ok(match parse_stamped_chunk(&req) {
                        // Echo the raw requested bytes so a malformed address is
                        // still correlatable by the client.
                        Err(status) => upload_error(req.address, status.message().to_string()),
                        Ok(stamped) => {
                            let address = *stamped.address();
                            // Honour the per-request validate flag, as the unary RPC does.
                            let result = if validate {
                                provider.send_chunk(stamped).await
                            } else {
                                provider.send_chunk_unchecked(stamped).await
                            };
                            match result {
                                Ok(receipt) => receipt_response(address, receipt),
                                Err(e) => upload_error(address.as_bytes().to_vec(), e.to_string()),
                            }
                        }
                    })
                }
            })
            .buffer_unordered(StreamConfig::DEFAULT.max_concurrency);

        Ok(Response::new(Box::pin(out)))
    }

    type RetrieveChunksStream = ResponseStream<RetrieveChunkResponse>;

    /// Retrieve a stream of chunks by address; responses return in completion
    /// order, each carrying its address. A per-address failure is one error
    /// item, not a teardown.
    ///
    /// Retrievals are driven directly off the inbound stream via
    /// `buffer_unordered`, bounding server memory by the concurrency rather than
    /// the (untrusted) request count. The native preset keeps enough forwarding
    /// retrievals in flight to saturate a bulk download.
    async fn retrieve_chunks(
        &self,
        request: Request<Streaming<RetrieveChunkRequest>>,
    ) -> Result<Response<Self::RetrieveChunksStream>, Status> {
        let inbound = request.into_inner();
        let provider = self.provider.clone();

        let out = inbound
            .map(move |item| {
                let provider = provider.clone();
                async move {
                    let req = item.map_err(|status| {
                        Status::internal(format!("inbound stream error: {status}"))
                    })?;
                    Ok(match parse_chunk_address(&req.address) {
                        // Echo the raw requested bytes; continue the stream.
                        Err(status) => retrieve_error(req.address, status.message().to_string()),
                        Ok(address) => match retrieve_verified(provider, address).await {
                            Ok(verified) => verified_response(address, verified),
                            Err(e) => retrieve_error(address.as_bytes().to_vec(), e.to_string()),
                        },
                    })
                }
            })
            .buffer_unordered(NATIVE_DOWNLOAD_CONCURRENCY);

        Ok(Response::new(Box::pin(out)))
    }

    type HasChunksStream = ResponseStream<HasChunkResponse>;

    /// Stream existence checks. `has_chunk` is a local sync lookup, so each
    /// request maps straight to a response carrying its address.
    async fn has_chunks(
        &self,
        request: Request<Streaming<HasChunkRequest>>,
    ) -> Result<Response<Self::HasChunksStream>, Status> {
        let inbound = request.into_inner();
        let provider = self.provider.clone();

        let out = inbound.map(move |item| {
            let req =
                item.map_err(|status| Status::internal(format!("inbound stream error: {status}")))?;
            let address = parse_chunk_address(&req.address)?;
            Ok(HasChunkResponse {
                address: address.as_bytes().to_vec(),
                exists: provider.has_chunk(&address),
            })
        });

        Ok(Response::new(Box::pin(out)))
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use vertex_swarm_api::{AnyChunk, Chunk as _, ContentChunk};

    use super::*;
    use crate::proto::chunk::ChunkType;

    /// Build a content chunk and return its wire encoding plus address.
    fn content_wire(data: &[u8]) -> (Vec<u8>, ChunkAddress) {
        let chunk: ContentChunk = ContentChunk::new(data.to_vec()).expect("valid content chunk");
        let address = *chunk.address();
        (Bytes::from(chunk).to_vec(), address)
    }

    fn content_request(data: Vec<u8>, address: &ChunkAddress) -> UploadChunkRequest {
        UploadChunkRequest {
            data,
            stamp: vec![0u8; 113],
            address: address.as_bytes().to_vec(),
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
        // Too short to be a 32-byte address.
        req.address = vec![0u8; 10];

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
