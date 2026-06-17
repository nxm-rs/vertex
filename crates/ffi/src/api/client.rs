//! The embedded client handle and its upload/download surface.
//!
//! [`VertexClient`] owns a native tokio runtime, the running client node task,
//! and the chunk provider that drives uploads and downloads. The host builds one
//! with [`VertexClient::build`], then calls [`VertexClient::upload_chunk`] and
//! [`VertexClient::download_chunk`]. Dropping the handle fires graceful shutdown
//! and tears the node down.

use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::B256;
use alloy_signer_local::PrivateKeySigner;
use flutter_rust_bridge::frb;
use futures::StreamExt;
use nectar_postage::Stamp;
use tokio::runtime::Runtime;
use vertex_node_api::InfrastructureContext;
use vertex_swarm_api::{
    ChunkAddress, HasChunkClient, Multiaddr, PushReceipt, StampedChunk, SwarmChunkProvider,
    SwarmError,
};
use vertex_swarm_builder::{
    ChunkVerifyConfig, ClientConfig, DefaultClientBuilder, NetworkChunkProvider,
    VerifyingChunkProvider,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_node::args::{ChainConfig, NetworkConfig, SwapConfig};
use vertex_swarm_primitives::{Nonce, SwarmNodeType};
use vertex_swarm_spec::{Spec, init_dev, init_mainnet, init_testnet};
use vertex_swarm_stream::{
    ChunkClientExt, GetStream, ParseAddressError, PutStream, StreamConfig,
    parse_address as core_parse_address, try_put_stream,
};
use vertex_tasks::{TaskExecutor, TaskManager};

use crate::api::types::{
    VertexChunkData, VertexChunkDownload, VertexChunkUpload, VertexClientConfig, VertexNetwork,
    VertexPushReceipt, VertexStreamConfig, VertexUploadAck,
};
use crate::error::{FfiError, FfiResult};

/// How long [`VertexClient`] waits for in-flight tasks during shutdown.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Network chunk provider wrapped with config-gated download verification, the
/// concrete provider the client builder produces for a client node.
type ClientChunks = VerifyingChunkProvider<NetworkChunkProvider<Arc<Identity>>>;

/// A running embedded Swarm client.
///
/// Opaque to the host: it is constructed and used only through the methods in
/// this module. It owns the runtime that drives the node so the host does not
/// have to manage one.
#[frb(opaque)]
pub struct VertexClient {
    chunks: ClientChunks,
    runtime: Runtime,
    // Held so the global executor and the node task stay alive for the lifetime
    // of the client. Taken on drop to fire graceful shutdown.
    task_manager: Option<TaskManager>,
}

impl VertexClient {
    /// Build and start an embedded client for `config`.
    ///
    /// Spins up a native multi-thread runtime, constructs the client node on it,
    /// and spawns the node's event loop as a background task. Returns once the
    /// node is built and running; the client begins discovering peers
    /// immediately.
    ///
    /// One client per process. The node internals resolve their task executor
    /// from a process-global slot that the first built client populates, so a
    /// second concurrent client would spawn its node tasks onto the first
    /// client's runtime. Build a single client, hold it for the process
    /// lifetime, and drop it to shut the node down.
    pub fn build(config: VertexClientConfig) -> Result<VertexClient, FfiError> {
        let runtime = Runtime::new().map_err(|e| FfiError::Build {
            reason: format!("runtime: {e}"),
        })?;

        // The task manager registers the global executor against the runtime
        // handle and owns the shutdown signal the node task observes.
        let task_manager = TaskManager::new(runtime.handle().clone());
        let executor = task_manager.executor();

        let spec = network_spec(config.network);
        let identity = build_identity(&spec, config.private_key.as_deref())?;
        let network = build_network(config.bootnodes);

        let node_config = ClientConfig::new(
            spec,
            identity,
            network,
            Default::default(),
            ChunkVerifyConfig::default(),
            ChainConfig::default(),
            SwapConfig::default(),
        );

        let launch = LaunchContext::new(executor.clone());

        let (task_fn, providers) = runtime
            .block_on(DefaultClientBuilder::from_config(node_config).build(&launch))
            .map_err(|e| FfiError::Build {
                reason: e.to_string(),
            })?
            .into_parts();

        let chunks = chunks_from(providers);
        executor.spawn_critical_with_graceful_shutdown_signal("vertex.ffi.node", task_fn);

        Ok(VertexClient {
            chunks,
            runtime,
            task_manager: Some(task_manager),
        })
    }

    /// Upload a pre-stamped chunk to the storers closest to its address.
    ///
    /// The chunk, its address, and its postage stamp are reconstructed into a
    /// strong [`StampedChunk`] before any network call. Returns the first
    /// storer's receipt.
    pub fn upload_chunk(&self, chunk: VertexChunkUpload) -> Result<VertexPushReceipt, FfiError> {
        let validate = chunk.validate;
        let stamped = reconstruct_upload(chunk)?;

        // `put` selects the stamp-signature check from the flag, collapsing the
        // former validate? branch onto the chunk core.
        let receipt = self.runtime.block_on(self.chunks.put(stamped, validate));

        receipt.map(receipt_into_ffi).map_err(|e| FfiError::Upload {
            reason: e.to_string(),
        })
    }

    /// Download the chunk at `address` from the network.
    ///
    /// `address` is the chunk's 32-byte address. `verify_stamp` opts into postage
    /// stamp signer recovery; chunk content integrity is always enforced by the
    /// retrieval path.
    pub fn download_chunk(
        &self,
        address: Vec<u8>,
        verify_stamp: bool,
    ) -> Result<VertexChunkDownload, FfiError> {
        let address = parse_address(&address)?;

        let result = self
            .runtime
            .block_on(self.chunks.retrieve_chunk(&address))
            .map_err(|e| FfiError::Download {
                reason: e.to_string(),
            })?;

        let served_by = result.served_by.to_string();

        // A storer may omit the stamp from a delivery; the chunk is still
        // address-validated. Verify only a present stamp, and emit an empty
        // stamp field when absent.
        if verify_stamp && let Some(stamp) = &result.stamp {
            stamp
                .recover_signer(&address)
                .map_err(|e| FfiError::Download {
                    reason: format!("stamp signature: {e}"),
                })?;
        }

        let stamp = result
            .stamp
            .map(|s| s.to_bytes().to_vec())
            .unwrap_or_default();

        Ok(VertexChunkDownload {
            data: result.chunk.into_bytes().to_vec(),
            stamp,
            served_by,
        })
    }

    /// Open a memory-bounded streaming download over a list of chunk addresses.
    ///
    /// Returns a pull-based [`VertexDownloadStream`] handle, not a pushed sink.
    /// The host drives it by awaiting [`VertexDownloadStream::next`] once per
    /// item; the core retrieval pipeline advances only when the host pulls, so a
    /// host that stops awaiting transitively pauses the network reads and nothing
    /// is buffered on the host's behalf. At most `config.max_concurrency` chunks
    /// are in flight. Items arrive in completion order, each carrying its
    /// address. A per-address failure (a miss, wrong bytes, or no candidate
    /// peer) arrives as an item carrying `error`, never as a torn-down stream.
    /// The returned [`FfiError`] only covers up-front input rejection (a
    /// malformed address); retrieval failures surface as items.
    pub fn download_stream(
        &self,
        addresses: Vec<Vec<u8>>,
        config: VertexStreamConfig,
    ) -> Result<VertexDownloadStream, FfiError> {
        // Reject malformed input up front so the host learns immediately rather
        // than mid-stream.
        let parsed: Vec<ChunkAddress> = addresses
            .iter()
            .map(|bytes| parse_address(bytes))
            .collect::<FfiResult<_>>()?;

        let cfg = stream_config(config);
        let inner = self.chunks.get_many(parsed, cfg);
        Ok(VertexDownloadStream {
            state: tokio::sync::Mutex::new(DownloadState { inner, index: 0 }),
        })
    }

    /// Open a memory-bounded streaming upload over a list of pre-stamped chunks.
    ///
    /// Returns a pull-based [`VertexUploadStream`] handle. The host drives it by
    /// awaiting [`VertexUploadStream::next`] once per chunk; the core push
    /// pipeline admits a new push only when the host pulls, up to
    /// `config.max_concurrency` at once, so a host that stops awaiting acks
    /// transitively pauses the network pushes. Each chunk is reconstructed into a
    /// strong [`StampedChunk`] lazily, as the pipeline admits it.
    ///
    /// A chunk whose bytes do not match its address fails at admission and
    /// surfaces as the ack item for that chunk carrying `error`; the stream then
    /// continues with the rest. Per-chunk push failures (no storer, rejection)
    /// surface the same way.
    pub fn upload_stream(
        &self,
        chunks: Vec<VertexChunkUpload>,
        config: VertexStreamConfig,
    ) -> Result<VertexUploadStream, FfiError> {
        let cfg = stream_config(config);
        // Parse each chunk's address up front so a malformed address fails before
        // any push starts, and so the feed can pair each chunk with its address.
        let addresses: Vec<ChunkAddress> = chunks
            .iter()
            .map(|chunk| parse_address(&chunk.address))
            .collect::<FfiResult<_>>()?;

        let sender = self.chunks.clone();
        // Reconstruct each chunk only as the pipeline pulls it. A reconstruction
        // failure surfaces as that address's error ack rather than aborting.
        let feed = addresses.into_iter().zip(chunks).map(|(address, chunk)| {
            let built = reconstruct_upload(chunk).map_err(|e| SwarmError::InvalidChunk {
                address: Some(address),
                reason: e.to_string(),
            });
            (address, built)
        });
        let inner = try_put_stream(sender, feed, cfg);
        Ok(VertexUploadStream {
            state: tokio::sync::Mutex::new(UploadState { inner, index: 0 }),
        })
    }
}

/// Mutable state of a [`VertexDownloadStream`]: the core stream and the next
/// item index. The address comes from each stream item, not by position.
struct DownloadState {
    inner: GetStream<ClientChunks>,
    index: u64,
}

/// A pull-based streaming download handle.
///
/// Opaque to the host: it holds the bounded core [`GetStream`] pipeline and is
/// driven one item at a time through [`Self::next`]. Because the core advances
/// only when polled, a host that awaits slowly paces the network reads and the
/// in-flight byte window is never exceeded; nothing accumulates on the host's
/// behalf. Dropping the handle drops the core stream and cancels its in-flight
/// retrievals.
///
/// The core stream's in-flight futures are `Send` but not `Sync`, while the
/// bridge requires the opaque handle to be `Sync`; the `tokio::sync::Mutex`
/// supplies that (`Mutex<T>: Sync` for `T: Send`) and is held across the single
/// `next` poll. The host drives one stream serially, so the lock is uncontended.
#[frb(opaque)]
pub struct VertexDownloadStream {
    state: tokio::sync::Mutex<DownloadState>,
}

impl VertexDownloadStream {
    /// Pull the next downloaded chunk, or `None` once every address has produced
    /// an item.
    ///
    /// Polls the core stream once. Items arrive in completion order, each
    /// carrying its address. Awaiting this is the backpressure: until the host
    /// calls it, the core issues no further retrievals.
    pub async fn next(&self) -> Option<VertexChunkData> {
        let mut state = self.state.lock().await;
        let (address, result) = state.inner.next().await?;
        let index = state.index;
        state.index += 1;
        let address = address.as_bytes().to_vec();
        Some(match result {
            Ok(verified) => {
                let (chunk, stamp) = verified.into_parts();
                // A storer may omit the stamp from a delivery; emit an empty
                // stamp field when absent.
                let stamp = stamp.map(|s| s.to_bytes().to_vec()).unwrap_or_default();
                VertexChunkData {
                    index,
                    address,
                    // One copy at the boundary; the chunk stayed `Bytes` until here.
                    data: chunk.into_bytes().to_vec(),
                    stamp,
                    error: None,
                }
            }
            Err(error) => VertexChunkData {
                index,
                address,
                data: Vec::new(),
                stamp: Vec::new(),
                error: Some(error.to_string()),
            },
        })
    }
}

/// Mutable state of a [`VertexUploadStream`]: the core stream and the next ack
/// index. The address comes from each stream item, not by position.
struct UploadState {
    inner: PutStream<ClientChunks>,
    index: u64,
}

/// A pull-based streaming upload handle.
///
/// Opaque to the host: it holds the bounded core [`PutStream`] pipeline and is
/// driven one ack at a time through [`Self::next`]. The core admits a push only
/// as the host pulls, so a host that stops awaiting acks pauses the pushes.
/// Dropping the handle drops the core stream and cancels its in-flight pushes.
///
/// The `tokio::sync::Mutex` makes the handle `Sync` for the bridge (the core
/// stream is `Send` but not `Sync`); it is held only across the single `next`
/// poll and is uncontended because the host drives one stream serially.
#[frb(opaque)]
pub struct VertexUploadStream {
    state: tokio::sync::Mutex<UploadState>,
}

impl VertexUploadStream {
    /// Pull the next upload ack, or `None` once every chunk has produced an ack.
    pub async fn next(&self) -> Option<VertexUploadAck> {
        let mut state = self.state.lock().await;
        let (address, result) = state.inner.next().await?;
        let index = state.index;
        state.index += 1;
        let address = address.as_bytes().to_vec();
        Some(match result {
            Ok(receipt) => VertexUploadAck {
                index,
                address,
                receipt: Some(receipt_into_ffi(receipt)),
                error: None,
            },
            Err(error) => VertexUploadAck {
                index,
                address,
                receipt: None,
                error: Some(error.to_string()),
            },
        })
    }
}

impl Drop for VertexClient {
    fn drop(&mut self) {
        if let Some(manager) = self.task_manager.take() {
            manager.graceful_shutdown_with_timeout(SHUTDOWN_TIMEOUT);
        }
    }
}

/// Minimal infrastructure context for building the client outside the CLI.
///
/// The launch path needs an executor and a data directory. The FFI client runs
/// fully in-memory: `db_path()` stays `None`, so no database is opened and no
/// peer snapshots are persisted. The data directory is a temporary path derived
/// from the system temp dir and is never written to by the launch path.
struct LaunchContext {
    executor: TaskExecutor,
    data_dir: std::path::PathBuf,
}

impl LaunchContext {
    fn new(executor: TaskExecutor) -> Self {
        Self {
            executor,
            data_dir: std::env::temp_dir().join("vertex-ffi-client"),
        }
    }
}

impl InfrastructureContext for LaunchContext {
    fn executor(&self) -> &TaskExecutor {
        &self.executor
    }

    fn data_dir(&self) -> &std::path::Path {
        &self.data_dir
    }
}

/// Map the boundary stream config to the core [`StreamConfig`].
///
/// Limiting is by chunk count: only `max_concurrency` is used. The host's
/// `window_bytes` is retained for ABI stability but ignored (byte/bandwidth
/// limiting belongs at the connection layer). The core clamps to at least one,
/// so a zero degrades to one-at-a-time streaming rather than a deadlock.
fn stream_config(config: VertexStreamConfig) -> StreamConfig {
    StreamConfig::new(usize::try_from(config.max_concurrency).unwrap_or(usize::MAX))
}

/// Resolve the spec for the requested network.
fn network_spec(network: VertexNetwork) -> Arc<Spec> {
    match network {
        VertexNetwork::Mainnet => init_mainnet(),
        VertexNetwork::Testnet => init_testnet(),
        VertexNetwork::Dev => init_dev(),
    }
}

/// Build a client identity from an optional private key.
///
/// A present key must be exactly 32 bytes. An absent key yields a random
/// ephemeral identity.
fn build_identity(spec: &Arc<Spec>, private_key: Option<&[u8]>) -> FfiResult<Arc<Identity>> {
    let Some(key) = private_key else {
        return Ok(Arc::new(Identity::random(
            spec.clone(),
            SwarmNodeType::Client,
        )));
    };

    if key.len() != 32 {
        return Err(FfiError::InvalidPrivateKey { len: key.len() });
    }

    let signer =
        PrivateKeySigner::from_bytes(&B256::from_slice(key)).map_err(|e| FfiError::Build {
            reason: format!("private key: {e}"),
        })?;

    Ok(Arc::new(Identity::new(
        signer,
        Nonce::random(),
        spec.clone(),
        SwarmNodeType::Client,
    )))
}

/// Build the network config, overriding bootnodes when the host supplies them.
fn build_network(bootnodes: Vec<String>) -> NetworkConfig {
    let mut network = NetworkConfig::default();
    if !bootnodes.is_empty() {
        let parsed: Vec<Multiaddr> = bootnodes
            .iter()
            .filter_map(|addr| addr.parse().ok())
            .collect();
        if !parsed.is_empty() {
            network.override_bootnodes(parsed);
        }
    }
    network
}

/// Reconstruct a strong [`StampedChunk`] from the raw upload payload.
///
/// Consumes the upload so the host-supplied `data` `Vec` moves straight into
/// `Bytes` (a zero-copy `Vec -> Bytes` conversion). This is the only payload
/// materialization on the upload-in direction: the bridge copies the host bytes
/// once into this `Vec`, and the conversion reuses that allocation, so no second
/// copy is made.
fn reconstruct_upload(chunk: VertexChunkUpload) -> FfiResult<StampedChunk> {
    let address = parse_address(&chunk.address)?;
    let stamp = parse_stamp(&chunk.stamp)?;
    // The bytes self-validate against the address (a mismatch is rejected), which
    // also pins the chunk variant.
    StampedChunk::reconstruct(address, chunk.data.into(), stamp).map_err(|e| {
        FfiError::ChunkMismatch {
            reason: e.to_string(),
        }
    })
}

/// Parse a 32-byte chunk address, mapping the core [`ParseAddressError`] onto the
/// FFI boundary's [`FfiError::InvalidAddress`].
fn parse_address(bytes: &[u8]) -> FfiResult<ChunkAddress> {
    core_parse_address(bytes)
        .map_err(|ParseAddressError { got }| FfiError::InvalidAddress { len: got })
}

/// Parse a wire-encoded postage stamp.
fn parse_stamp(bytes: &[u8]) -> FfiResult<Stamp> {
    Stamp::try_from_slice(bytes).map_err(|e| FfiError::InvalidStamp {
        reason: e.to_string(),
    })
}

/// Map a [`PushReceipt`] to the flat boundary shape.
fn receipt_into_ffi(receipt: PushReceipt) -> VertexPushReceipt {
    let PushReceipt {
        storer,
        signature,
        nonce,
        storage_radius,
    } = receipt;
    VertexPushReceipt {
        storer: storer.to_string(),
        signature: signature.as_bytes().to_vec(),
        nonce: nonce.as_slice().to_vec(),
        storage_radius: u32::from(storage_radius.get()),
    }
}

/// Extract the chunk provider from the built client's components.
fn chunks_from(components: impl HasChunkClient<ChunkClient = ClientChunks>) -> ClientChunks {
    components.chunk_client().clone()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use vertex_swarm_api::{AnyChunk, Chunk, ContentChunk};

    use super::*;

    /// Build a content chunk from raw data and return its wire encoding alongside
    /// its address, matching the upload payload the boundary expects.
    fn content_wire(data: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let chunk: ContentChunk = ContentChunk::new(data.to_vec()).unwrap();
        let address = chunk.address().as_bytes().to_vec();
        let wire = AnyChunk::Content(chunk).into_bytes().to_vec();
        (wire, address)
    }

    fn upload(data: Vec<u8>, address: Vec<u8>, stamp: Vec<u8>) -> VertexChunkUpload {
        VertexChunkUpload {
            address,
            data,
            stamp,
            validate: false,
        }
    }

    #[test]
    fn reconstruct_accepts_matching_address() {
        let (wire, address) = content_wire(b"reconstruct me");
        let stamped = reconstruct_upload(upload(wire, address.clone(), vec![0u8; 113])).unwrap();
        assert_eq!(stamped.address().as_bytes(), address.as_slice());
        assert!(stamped.chunk().is_content());
    }

    #[test]
    fn reconstruct_rejects_short_address() {
        let (wire, _) = content_wire(b"payload");
        let err = reconstruct_upload(upload(wire, vec![0u8; 4], vec![0u8; 113])).unwrap_err();
        assert!(matches!(err, FfiError::InvalidAddress { len: 4 }));
    }

    #[test]
    fn reconstruct_rejects_short_stamp() {
        let (wire, address) = content_wire(b"payload");
        let err = reconstruct_upload(upload(wire, address, vec![0u8; 10])).unwrap_err();
        assert!(matches!(err, FfiError::InvalidStamp { .. }));
    }

    #[test]
    fn reconstruct_rejects_address_mismatch() {
        let (wire, _) = content_wire(b"payload");
        let wrong = vec![0xabu8; 32];
        let err = reconstruct_upload(upload(wire, wrong, vec![0u8; 113])).unwrap_err();
        assert!(matches!(err, FfiError::ChunkMismatch { .. }));
    }

    #[test]
    fn build_identity_rejects_wrong_key_length() {
        let spec = init_dev();
        let result = build_identity(&spec, Some(&[0u8; 16]));
        assert!(matches!(
            result,
            Err(FfiError::InvalidPrivateKey { len: 16 })
        ));
    }

    #[test]
    fn build_identity_without_key_is_ephemeral() {
        use vertex_swarm_api::SwarmIdentity;
        let spec = init_dev();
        let identity = build_identity(&spec, None).expect("ephemeral identity builds");
        assert_eq!(identity.node_type(), SwarmNodeType::Client);
    }
}
