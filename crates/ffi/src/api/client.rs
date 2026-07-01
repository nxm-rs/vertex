//! The embedded client handle and its upload/download surface.
//!
//! [`VertexClient`] owns a native tokio runtime, the running node task, and the
//! chunk provider for uploads and downloads. Dropping the handle fires graceful
//! shutdown.

use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::B256;
use alloy_signer_local::PrivateKeySigner;
use flutter_rust_bridge::frb;
use futures::StreamExt;
use nectar_postage::Stamp;
use tokio::runtime::Runtime;
use vertex_node_builder::NodeBuilder;
use vertex_node_core::dirs::DataDirs;
use vertex_swarm_api::{
    ChunkAddress, HasChunkClient, Multiaddr, PushReceipt, StampedChunk, SwarmChunkProvider,
    SwarmError,
};
use vertex_swarm_builder::{ClientConfig, NativeChunkProvider};
use vertex_swarm_identity::Identity;
use vertex_swarm_node::args::{ChainConfig, NetworkConfig, SwapConfig};
use vertex_swarm_primitives::{Nonce, SwarmNodeType};
use vertex_swarm_spec::{Spec, init_dev, init_mainnet, init_testnet};
use vertex_swarm_stream::{
    ChunkClientExt, GetStream, ParseAddressError, PutStream, StreamConfig,
    parse_address as core_parse_address, try_put_stream,
};
use vertex_tasks::TaskManager;

use crate::api::types::{
    VertexChunkData, VertexChunkDownload, VertexChunkUpload, VertexClientConfig, VertexNetwork,
    VertexPushReceipt, VertexStreamConfig, VertexUploadAck,
};
use crate::error::{FfiError, FfiResult};

/// How long [`VertexClient`] waits for in-flight tasks during shutdown.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// The concrete chunk provider the client builder produces for a client node.
type ClientChunks = NativeChunkProvider;

/// A running embedded Swarm client. Opaque to the host; owns the runtime that
/// drives the node.
#[frb(opaque)]
pub struct VertexClient {
    chunks: ClientChunks,
    runtime: Runtime,
    // Keeps the global executor and node task alive; taken on drop to shut down.
    task_manager: Option<TaskManager>,
}

impl VertexClient {
    /// Build and start an embedded client for `config`, returning once the node
    /// is running and discovering peers.
    ///
    /// One client per process: node internals resolve their executor from a
    /// process-global slot the first client populates, so a second concurrent
    /// client would spawn onto the first client's runtime.
    pub fn build(config: VertexClientConfig) -> Result<VertexClient, FfiError> {
        let runtime = Runtime::new().map_err(|e| FfiError::Build {
            reason: format!("runtime: {e}"),
        })?;

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
            Default::default(),
            ChainConfig::default(),
            SwapConfig::default(),
        );

        // Launch through the node-builder shell without a gRPC server: the node
        // task is spawned internally on `executor`, and the bare client
        // components come back in the handle.
        let handle = runtime
            .block_on(
                NodeBuilder::new()
                    .with_launch_context((), executor, client_data_dirs())
                    .with_protocol(node_config)
                    .launch_without_grpc(),
            )
            .map_err(|e| FfiError::Build {
                reason: e.to_string(),
            })?;

        // The selector-aware chunk provider the client components hold.
        let chunks: ClientChunks = handle.components().chunk_client().clone();

        Ok(VertexClient {
            chunks,
            runtime,
            task_manager: Some(task_manager),
        })
    }

    /// Upload a pre-stamped chunk to the storers closest to its address,
    /// returning the first storer's receipt.
    pub fn upload_chunk(&self, chunk: VertexChunkUpload) -> Result<VertexPushReceipt, FfiError> {
        let validate = chunk.validate;
        let stamped = reconstruct_upload(chunk)?;

        let receipt = self.runtime.block_on(self.chunks.put(stamped, validate));

        receipt.map(receipt_into_ffi).map_err(|e| FfiError::Upload {
            reason: e.to_string(),
        })
    }

    /// Download the chunk at the 32-byte `address`.
    ///
    /// `verify_stamp` opts into postage stamp signer recovery; chunk content
    /// integrity is always enforced by the retrieval path.
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

        // A delivery may omit the stamp; the chunk is still address-validated.
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
    /// At most `config.max_concurrency` chunks are in flight, arriving in
    /// completion order. A per-address failure surfaces as an item carrying
    /// `error`, not a torn-down stream; the returned [`FfiError`] only covers a
    /// malformed address rejected up front.
    pub fn download_stream(
        &self,
        addresses: Vec<Vec<u8>>,
        config: VertexStreamConfig,
    ) -> Result<VertexDownloadStream, FfiError> {
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
    /// Up to `config.max_concurrency` pushes are in flight; each chunk is
    /// reconstructed lazily as admitted. A per-chunk failure (address mismatch,
    /// no storer, rejection) surfaces as that chunk's ack carrying `error`; the
    /// stream continues.
    pub fn upload_stream(
        &self,
        chunks: Vec<VertexChunkUpload>,
        config: VertexStreamConfig,
    ) -> Result<VertexUploadStream, FfiError> {
        let cfg = stream_config(config);
        // Parse all addresses before any push, so a malformed one fails up front.
        let addresses: Vec<ChunkAddress> = chunks
            .iter()
            .map(|chunk| parse_address(&chunk.address))
            .collect::<FfiResult<_>>()?;

        let sender = self.chunks.clone();
        // Lazy per-chunk reconstruction: a failure becomes an error ack, not an abort.
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

struct DownloadState {
    inner: GetStream<ClientChunks>,
    index: u64,
}

/// A pull-based streaming download handle, opaque to the host and driven one
/// item at a time through [`Self::next`]. Dropping it cancels in-flight
/// retrievals.
///
/// The `tokio::sync::Mutex` makes the handle `Sync` for the bridge (the core
/// stream's in-flight futures are `Send` but not `Sync`); held only across the
/// single `next` poll, uncontended since the host drives serially.
#[frb(opaque)]
pub struct VertexDownloadStream {
    state: tokio::sync::Mutex<DownloadState>,
}

impl VertexDownloadStream {
    /// Pull the next downloaded chunk in completion order, or `None` once every
    /// address has produced an item. Awaiting this is the backpressure.
    pub async fn next(&self) -> Option<VertexChunkData> {
        let mut state = self.state.lock().await;
        let (address, result) = state.inner.next().await?;
        let index = state.index;
        state.index += 1;
        let address = address.as_bytes().to_vec();
        Some(match result {
            Ok(verified) => {
                let (chunk, stamp) = verified.into_parts();
                let stamp = stamp.map(|s| s.to_bytes().to_vec()).unwrap_or_default();
                VertexChunkData {
                    index,
                    address,
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

struct UploadState {
    inner: PutStream<ClientChunks>,
    index: u64,
}

/// A pull-based streaming upload handle, opaque to the host and driven one ack
/// at a time through [`Self::next`]. Dropping it cancels in-flight pushes.
///
/// The `tokio::sync::Mutex` makes the handle `Sync` for the bridge (the core
/// stream is `Send` but not `Sync`); held only across the single `next` poll,
/// uncontended since the host drives serially.
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

/// Data directories for the embedded client: a temp path the in-memory node uses
/// only for its launch log line (no database is opened, `db_path` stays `None`).
fn client_data_dirs() -> DataDirs {
    DataDirs::ephemeral(std::env::temp_dir().join("vertex-ffi-client"))
}

/// Map the boundary stream config to the core [`StreamConfig`].
///
/// Limiting is by chunk count; `window_bytes` is retained for ABI stability but
/// ignored. The core clamps to at least one, so a zero degrades to
/// one-at-a-time rather than deadlocking.
fn stream_config(config: VertexStreamConfig) -> StreamConfig {
    StreamConfig::new(usize::try_from(config.max_concurrency).unwrap_or(usize::MAX))
}

fn network_spec(network: VertexNetwork) -> Arc<Spec> {
    match network {
        VertexNetwork::Mainnet => init_mainnet(),
        VertexNetwork::Testnet => init_testnet(),
        VertexNetwork::Dev => init_dev(),
    }
}

/// Build a client identity. A present key must be exactly 32 bytes; an absent
/// key yields a random ephemeral identity.
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
/// Consumes the upload so `data` moves into `Bytes` without a second copy.
fn reconstruct_upload(chunk: VertexChunkUpload) -> FfiResult<StampedChunk> {
    let address = parse_address(&chunk.address)?;
    let stamp = parse_stamp(&chunk.stamp)?;
    // Bytes self-validate against the address (mismatch rejected), pinning the variant.
    StampedChunk::reconstruct(address, chunk.data.into(), stamp).map_err(|e| {
        FfiError::ChunkMismatch {
            reason: e.to_string(),
        }
    })
}

/// Parse a 32-byte chunk address.
fn parse_address(bytes: &[u8]) -> FfiResult<ChunkAddress> {
    core_parse_address(bytes)
        .map_err(|ParseAddressError { got }| FfiError::InvalidAddress { len: got })
}

fn parse_stamp(bytes: &[u8]) -> FfiResult<Stamp> {
    Stamp::try_from_slice(bytes).map_err(|e| FfiError::InvalidStamp {
        reason: e.to_string(),
    })
}

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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use vertex_swarm_api::{AnyChunk, Chunk, ContentChunk};

    use super::*;

    /// Build a content chunk and return its wire encoding alongside its address.
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
