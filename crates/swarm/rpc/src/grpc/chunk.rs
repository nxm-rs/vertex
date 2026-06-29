//! Chunk service: retrieval, upload, and existence checks over Swarm.

use std::pin::Pin;

use crate::proto::chunk::{
    ChunkError, HasChunkRequest, HasChunkResponse, RetrieveChunkRequest, RetrieveChunkResponse,
    RetrievedChunk, UploadChunkRequest, UploadChunkResponse, UploadReceipt, chunk_server::Chunk,
    retrieve_chunk_response, upload_chunk_response,
};
use futures::StreamExt;
use tonic::{Request, Response, Status, Streaming};
use vertex_swarm_api::{ChunkAddress, PushReceipt, Stamp, StampedChunk, SwarmError};
use vertex_swarm_stream::{
    ChunkClient, ChunkClientExt, StreamConfig, VerifiedChunk, get_stream_from, parse_address,
};

/// Server-side policy for the caller-controlled per-request `validate` flag. A
/// public endpoint must not let a caller skip stamp-signature validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StampValidation {
    /// Always validate, ignoring the request flag. Default for public endpoints.
    #[default]
    Enforce,
    /// Honour the request's `validate` flag. For trusted/private endpoints.
    PerRequest,
}

impl StampValidation {
    /// Effective validate decision for a request whose flag is `requested`.
    #[must_use]
    pub fn resolve(self, requested: bool) -> bool {
        match self {
            Self::Enforce => true,
            Self::PerRequest => requested,
        }
    }
}

/// gRPC chunk retrieval and upload service.
pub struct ChunkService<P> {
    provider: P,
    stamp_validation: StampValidation,
    connected_peers: std::sync::Arc<dyn Fn() -> usize + Send + Sync>,
}

impl<P> ChunkService<P> {
    /// Defaults to [`StampValidation::Enforce`] and a zero peer count (the
    /// download pipeline depth falls back to its floor until a live peer-count
    /// source is wired in).
    pub fn new(provider: P) -> Self {
        Self {
            provider,
            stamp_validation: StampValidation::Enforce,
            connected_peers: std::sync::Arc::new(|| 0),
        }
    }

    #[must_use]
    pub fn with_stamp_validation(mut self, stamp_validation: StampValidation) -> Self {
        self.stamp_validation = stamp_validation;
        self
    }

    /// Wire a live connected-peer count so the download pipeline depth scales
    /// with the connected peer set.
    #[must_use]
    pub fn with_peer_count(
        mut self,
        connected_peers: std::sync::Arc<dyn Fn() -> usize + Send + Sync>,
    ) -> Self {
        self.connected_peers = connected_peers;
        self
    }
}

#[allow(clippy::result_large_err)]
fn parse_chunk_address(bytes: &[u8]) -> Result<ChunkAddress, Status> {
    parse_address(bytes).map_err(|e| Status::invalid_argument(e.to_string()))
}

/// Build a [`StampedChunk`], rejecting bytes that do not hash to the supplied
/// address (which also pins the chunk variant).
#[allow(clippy::result_large_err)]
fn parse_stamped_chunk(req: &UploadChunkRequest) -> Result<StampedChunk, Status> {
    let address = parse_chunk_address(&req.address)?;

    let stamp = Stamp::try_from_slice(&req.stamp)
        .map_err(|e| Status::invalid_argument(format!("invalid postage stamp: {e}")))?;

    StampedChunk::reconstruct(address, req.data.clone().into(), stamp).map_err(|e| {
        Status::invalid_argument(format!("chunk does not match the supplied address: {e}"))
    })
}

/// Absence becomes `not_found`, all else `internal`.
#[allow(clippy::result_large_err)]
fn retrieval_status(error: &SwarmError) -> Status {
    match error {
        SwarmError::ChunkNotFound { .. } | SwarmError::NoStorer { .. } => {
            Status::not_found(format!("chunk not found: {error}"))
        }
        other => Status::internal(format!("chunk retrieval failed: {other}")),
    }
}

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

/// `address` is the raw requested bytes, echoed for correlation even when they
/// failed to parse.
fn retrieve_error(address: Vec<u8>, message: String) -> RetrieveChunkResponse {
    RetrieveChunkResponse {
        result: Some(retrieve_chunk_response::Result::Error(ChunkError {
            address,
            message,
        })),
    }
}

fn verified_response(address: ChunkAddress, verified: VerifiedChunk) -> RetrieveChunkResponse {
    let served_by = verified.served_by().as_bytes().to_vec();
    let (chunk, stamp) = verified.into_parts();
    retrieved_response(
        address,
        chunk.into_bytes().to_vec(),
        stamp.map(|s| s.to_bytes().to_vec()).unwrap_or_default(),
        served_by,
    )
}

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

/// `address` is the raw request bytes, echoed for correlation even when they
/// failed to parse.
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
impl<P: ChunkClient> Chunk for ChunkService<P> {
    async fn retrieve_chunk(
        &self,
        request: Request<RetrieveChunkRequest>,
    ) -> Result<Response<RetrieveChunkResponse>, Status> {
        let req = request.into_inner();
        let address = parse_chunk_address(&req.address)?;

        // `get` verifies the bytes answer the address, so wrong bytes error.
        match self.provider.get(address).await {
            Ok(verified) => Ok(Response::new(verified_response(address, verified))),
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

        let validate = self.stamp_validation.resolve(req.validate);
        let receipt = self
            .provider
            .put(stamped, validate)
            .await
            .map_err(|e| Status::internal(format!("chunk upload failed: {e}")))?;

        Ok(Response::new(receipt_response(address, receipt)))
    }

    type UploadChunksStream = ResponseStream<UploadChunkResponse>;

    /// Receipts return in completion order; a per-chunk failure is one error
    /// item, not a teardown. `buffer_unordered` pulls a new request only as a
    /// push slot frees, bounding server memory by the concurrency rather than
    /// the untrusted request count.
    async fn upload_chunks(
        &self,
        request: Request<Streaming<UploadChunkRequest>>,
    ) -> Result<Response<Self::UploadChunksStream>, Status> {
        let inbound = request.into_inner();
        let provider = self.provider.clone();
        let stamp_validation = self.stamp_validation;

        let out = inbound
            .map(move |item| {
                let provider = provider.clone();
                async move {
                    let req = item.map_err(|status| {
                        Status::internal(format!("inbound stream error: {status}"))
                    })?;
                    let validate = stamp_validation.resolve(req.validate);
                    Ok(match parse_stamped_chunk(&req) {
                        // Echo the raw bytes so a malformed address stays correlatable.
                        Err(status) => upload_error(req.address, status.message().to_string()),
                        Ok(stamped) => {
                            let address = *stamped.address();
                            match provider.put(stamped, validate).await {
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

    /// Per-address failure is one error item, not a teardown. Valid addresses
    /// route through the bounded, verify-by-default [`get_stream_from`] prefetch
    /// shared with the FFI and wasm download paths; malformed addresses and
    /// inbound-stream errors interleave as their own error items.
    ///
    /// The in-flight cap is derived from the connected peer set (peers times the
    /// per-peer cap, clamped), so the Kademlia peer count is the throughput
    /// lever rather than a fixed number; the node's per-peer cap plus skip-busy
    /// remain the true fan-out limiter.
    async fn retrieve_chunks(
        &self,
        request: Request<Streaming<RetrieveChunkRequest>>,
    ) -> Result<Response<Self::RetrieveChunksStream>, Status> {
        let inbound = request.into_inner();

        let config = StreamConfig::peer_bounded((self.connected_peers)());

        // Side channel for errors that never reach the prefetch (malformed
        // bytes, inbound errors). Bounded so a flood of bad requests cannot
        // outrun a slow consumer: `send` parks the parser, back-pressuring the
        // inbound reads.
        let (err_tx, err_rx) =
            tokio::sync::mpsc::channel::<RetrieveChunkResponse>(config.max_concurrency);

        let addresses = inbound.filter_map(move |item| {
            let err_tx = err_tx.clone();
            async move {
                match item {
                    Err(status) => {
                        let _ = err_tx
                            .send(retrieve_error(
                                Vec::new(),
                                format!("inbound stream error: {status}"),
                            ))
                            .await;
                        None
                    }
                    Ok(req) => match parse_chunk_address(&req.address) {
                        Ok(address) => Some(address),
                        Err(status) => {
                            let _ = err_tx
                                .send(retrieve_error(req.address, status.message().to_string()))
                                .await;
                            None
                        }
                    },
                }
            }
        });

        let verified =
            get_stream_from(self.provider.clone(), addresses, config).map(|(address, result)| {
                match result {
                    Ok(verified) => verified_response(address, verified),
                    Err(e) => retrieve_error(address.as_bytes().to_vec(), e.to_string()),
                }
            });

        let errors = tokio_stream::wrappers::ReceiverStream::new(err_rx);
        let out = futures::stream::select(verified, errors).map(Ok);
        Ok(Response::new(Box::pin(out)))
    }

    type HasChunksStream = ResponseStream<HasChunkResponse>;

    /// `has_chunk` is a local sync lookup, so each request maps straight to a
    /// response.
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

    #[test]
    fn stamp_validation_resolves_per_policy() {
        assert!(StampValidation::Enforce.resolve(false));
        assert!(StampValidation::Enforce.resolve(true));
        assert!(!StampValidation::PerRequest.resolve(false));
        assert!(StampValidation::PerRequest.resolve(true));
        assert_eq!(StampValidation::default(), StampValidation::Enforce);
    }

    /// A verified item maps onto the wire response with a populated `served_by`,
    /// driving the same `get_stream_from` core the RPC routes through.
    #[tokio::test]
    async fn retrieve_chunks_emits_served_by() {
        use vertex_swarm_api::{ChunkRetrievalResult, OverlayAddress, SwarmResult};
        use vertex_swarm_stream::{StreamConfig, get_stream_from};

        const SERVED_BY: [u8; 32] = [0x5b; 32];

        #[derive(Clone)]
        struct OneChunkProvider {
            chunk: AnyChunk,
        }

        #[tonic::async_trait]
        impl vertex_swarm_api::SwarmChunkProvider for OneChunkProvider {
            async fn retrieve_chunk(
                &self,
                _address: &ChunkAddress,
            ) -> SwarmResult<ChunkRetrievalResult> {
                Ok(ChunkRetrievalResult {
                    chunk: self.chunk.clone(),
                    stamp: None,
                    served_by: OverlayAddress::from(SERVED_BY),
                })
            }

            fn has_chunk(&self, _address: &ChunkAddress) -> bool {
                false
            }
        }

        let content = ContentChunk::new(b"served chunk".to_vec()).expect("valid content chunk");
        let address = *content.address();
        let provider = OneChunkProvider {
            chunk: AnyChunk::Content(content),
        };

        let mut out = get_stream_from(
            provider,
            futures::stream::iter(vec![address]),
            StreamConfig::peer_bounded(1),
        );
        let (item_address, result) = out.next().await.expect("one item");
        let verified = result.expect("retrieval verifies");
        let response = verified_response(item_address, verified);

        let Some(retrieve_chunk_response::Result::Chunk(chunk)) = response.result else {
            panic!("expected a chunk response");
        };
        assert_eq!(
            chunk.served_by,
            SERVED_BY.to_vec(),
            "served_by is populated"
        );
        assert_eq!(chunk.address, address.as_bytes().to_vec());
    }
}
