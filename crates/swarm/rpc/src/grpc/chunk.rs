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
    ChunkClient, ChunkClientExt, NATIVE_DOWNLOAD_CONCURRENCY, StreamConfig, VerifiedChunk,
    get_stream_from, parse_address,
};

/// Whether the chunk service trusts a caller's per-request `validate` flag or
/// always validates the postage stamp signature on upload.
///
/// The `validate` flag on an upload request is caller-controlled, so on a
/// publicly reachable endpoint a caller could set `validate = false` and have
/// the node forward a chunk carrying an unverified (or forged) stamp. This
/// policy moves that decision server-side: a public endpoint enforces
/// validation; a private/trusted endpoint may honour the per-request flag.
/// Embedded callers (FFI, wasm) trust the implementer and do not pass through
/// this service, so they keep honouring their own flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StampValidation {
    /// Always validate the stamp signature, ignoring the request's `validate`
    /// flag. The safe default for a publicly reachable gRPC endpoint.
    #[default]
    Enforce,
    /// Honour the request's `validate` flag (trust by default, opt in to
    /// validation). For a private or otherwise trusted endpoint.
    PerRequest,
}

impl StampValidation {
    /// The effective validate decision for a request whose flag is `requested`.
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
}

impl<P> ChunkService<P> {
    /// Create a new chunk service. Defaults to [`StampValidation::Enforce`]: a
    /// gRPC endpoint validates stamps unless an operator opts into trusting the
    /// per-request flag via [`Self::with_stamp_validation`].
    pub fn new(provider: P) -> Self {
        Self {
            provider,
            stamp_validation: StampValidation::Enforce,
        }
    }

    /// Set the stamp-validation policy (e.g. [`StampValidation::PerRequest`] for
    /// a trusted/private endpoint).
    #[must_use]
    pub fn with_stamp_validation(mut self, stamp_validation: StampValidation) -> Self {
        self.stamp_validation = stamp_validation;
        self
    }
}

/// Parse a 32-byte chunk address, mapping the core [`parse_address`] error to a
/// gRPC `invalid_argument` status.
#[allow(clippy::result_large_err)]
fn parse_chunk_address(bytes: &[u8]) -> Result<ChunkAddress, Status> {
    parse_address(bytes).map_err(|e| Status::invalid_argument(e.to_string()))
}

/// Build a [`StampedChunk`] from an upload request: the bytes self-validate
/// against the supplied address (an address that does not match the bytes is
/// rejected, which also pins the chunk variant).
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

/// Map a verified download item onto the wire response, carrying the real
/// `served_by` the streaming retrieve path now threads through (previously
/// emitted empty).
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
impl<P: ChunkClient> Chunk for ChunkService<P> {
    async fn retrieve_chunk(
        &self,
        request: Request<RetrieveChunkRequest>,
    ) -> Result<Response<RetrieveChunkResponse>, Status> {
        let req = request.into_inner();
        let address = parse_chunk_address(&req.address)?;

        // Verify-by-default: `get` proves the bytes answer the address before
        // responding, so a wrong-bytes delivery errors instead of returning.
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

        // The endpoint's policy resolves the effective decision from the caller's
        // flag: a public endpoint (Enforce) always validates the stamp.
        let validate = self.stamp_validation.resolve(req.validate);
        let receipt = self
            .provider
            .put(stamped, validate)
            .await
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
        let stamp_validation = self.stamp_validation;

        let out = inbound
            .map(move |item| {
                let provider = provider.clone();
                async move {
                    let req = item.map_err(|status| {
                        Status::internal(format!("inbound stream error: {status}"))
                    })?;
                    // Apply the endpoint policy, same as the unary RPC.
                    let validate = stamp_validation.resolve(req.validate);
                    Ok(match parse_stamped_chunk(&req) {
                        // Echo the raw requested bytes so a malformed address is
                        // still correlatable by the client.
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

    /// Retrieve a stream of chunks by address; responses return in completion
    /// order, each carrying its address and its serving overlay. A per-address
    /// failure is one error item, not a teardown.
    ///
    /// Valid addresses route through the core [`get_stream_from`], so this path
    /// shares one bounded, verify-by-default prefetch with the FFI and wasm
    /// download paths instead of a hand-rolled `buffer_unordered`. The native
    /// preset keeps enough forwarding retrievals in flight to saturate a bulk
    /// download. Malformed addresses and inbound-stream errors are emitted as
    /// their own error items, interleaved with the verified deliveries.
    async fn retrieve_chunks(
        &self,
        request: Request<Streaming<RetrieveChunkRequest>>,
    ) -> Result<Response<Self::RetrieveChunksStream>, Status> {
        let inbound = request.into_inner();

        // Malformed addresses and inbound-stream errors are not addresses the
        // core can retrieve, but must still surface as their own error items
        // rather than tear the stream down. They go onto this channel as a side
        // effect of parsing, and are interleaved with the core's deliveries
        // below.
        //
        // The channel is *bounded*: malformed requests never occupy a core
        // prefetch slot, so an unbounded channel would let a client streaming
        // nothing but bad requests faster than the consumer drains responses
        // grow the heap without limit (the core's prefetch only gates valid
        // addresses). A bounded channel makes `send` park the parser when the
        // consumer is slow, which transitively pauses the inbound reads, capping
        // buffered errors at the same chunk-count order as the valid-address
        // prefetch.
        let (err_tx, err_rx) =
            tokio::sync::mpsc::channel::<RetrieveChunkResponse>(NATIVE_DOWNLOAD_CONCURRENCY);

        // Parse the inbound feed into the core's address source, diverting every
        // non-address (malformed bytes, inbound error) onto the error channel.
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
                        // Echo the raw requested bytes for client-side correlation.
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

        // Valid addresses route through the core: one bounded, verify-by-default
        // prefetch shared with the FFI and wasm download paths.
        let verified = get_stream_from(
            self.provider.clone(),
            addresses,
            StreamConfig::NATIVE_DOWNLOAD,
        )
        .map(|(address, result)| match result {
            Ok(verified) => verified_response(address, verified),
            Err(e) => retrieve_error(address.as_bytes().to_vec(), e.to_string()),
        });

        let errors = tokio_stream::wrappers::ReceiverStream::new(err_rx);
        let out = futures::stream::select(verified, errors).map(Ok);
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

    #[test]
    fn stamp_validation_resolves_per_policy() {
        // Enforce always validates, ignoring the caller's flag; PerRequest
        // honours it. The default is the safe Enforce.
        assert!(StampValidation::Enforce.resolve(false));
        assert!(StampValidation::Enforce.resolve(true));
        assert!(!StampValidation::PerRequest.resolve(false));
        assert!(StampValidation::PerRequest.resolve(true));
        assert_eq!(StampValidation::default(), StampValidation::Enforce);
    }

    /// Streaming retrieve now threads the serving overlay through: a verified
    /// item maps onto the wire response with a populated `served_by`, where the
    /// streaming path previously emitted it empty. This drives the same
    /// `get_stream_from` core the RPC routes through.
    #[tokio::test]
    async fn retrieve_chunks_emits_served_by() {
        use vertex_swarm_api::{ChunkRetrievalResult, OverlayAddress, SwarmResult};
        use vertex_swarm_stream::{StreamConfig, get_stream_from};

        const SERVED_BY: [u8; 32] = [0x5b; 32];

        /// Provider serving one known content chunk from a fixed overlay.
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

        // Route a single address through the very core `retrieve_chunks` uses,
        // then map it the same way the RPC does.
        let mut out = get_stream_from(
            provider,
            futures::stream::iter(vec![address]),
            StreamConfig::NATIVE_DOWNLOAD,
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
