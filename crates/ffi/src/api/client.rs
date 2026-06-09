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
use nectar_postage::Stamp;
use tokio::runtime::Runtime;
use vertex_node_api::InfrastructureContext;
use vertex_swarm_api::{
    ChunkAddress, Multiaddr, PushReceipt, StampedChunk, SwarmChunkProvider, SwarmChunkSender,
};
use vertex_swarm_builder::{
    ChunkVerifyConfig, ClientConfig, ClientRpcProviders, DefaultClientBuilder,
    NetworkChunkProvider, VerifyingChunkProvider,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_node::args::{ChainConfig, NetworkConfig};
use vertex_swarm_primitives::{Nonce, SwarmNodeType};
use vertex_swarm_spec::{Spec, init_dev, init_mainnet, init_testnet};
use vertex_tasks::{TaskExecutor, TaskManager};

use crate::api::types::{
    VertexChunkDownload, VertexChunkUpload, VertexClientConfig, VertexNetwork, VertexPushReceipt,
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
    pub fn build(config: VertexClientConfig) -> FfiResult<VertexClient> {
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
    pub fn upload_chunk(&self, chunk: VertexChunkUpload) -> FfiResult<VertexPushReceipt> {
        let stamped = reconstruct_upload(&chunk)?;
        let validate = chunk.validate;

        let receipt = self.runtime.block_on(async {
            if validate {
                self.chunks.send_chunk(stamped).await
            } else {
                self.chunks.send_chunk_unchecked(stamped).await
            }
        });

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
    ) -> FfiResult<VertexChunkDownload> {
        let address = parse_address(&address)?;

        let result = self
            .runtime
            .block_on(self.chunks.retrieve_chunk(&address))
            .map_err(|e| FfiError::Download {
                reason: e.to_string(),
            })?;

        let served_by = result.served_by.to_string();
        let (chunk, stamp) = result.chunk.into_parts();

        if verify_stamp {
            stamp
                .recover_signer(&address)
                .map_err(|e| FfiError::Download {
                    reason: format!("stamp signature: {e}"),
                })?;
        }

        Ok(VertexChunkDownload {
            data: chunk.into_bytes().to_vec(),
            stamp: stamp.to_bytes().to_vec(),
            served_by,
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
/// without persistent on-disk state, so the data directory is a temporary path
/// derived from the system temp dir; the peer store falls back to ephemeral when
/// the database cannot be opened there.
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
            network.set_bootnodes(parsed);
        }
    }
    network
}

/// Reconstruct a strong [`StampedChunk`] from the raw upload payload.
fn reconstruct_upload(chunk: &VertexChunkUpload) -> FfiResult<StampedChunk> {
    let address = parse_address(&chunk.address)?;
    let stamp = parse_stamp(&chunk.stamp)?;
    StampedChunk::reconstruct(address, chunk.data.clone().into(), stamp).map_err(|e| {
        FfiError::ChunkMismatch {
            reason: e.to_string(),
        }
    })
}

/// Parse a 32-byte chunk address.
fn parse_address(bytes: &[u8]) -> FfiResult<ChunkAddress> {
    ChunkAddress::from_slice(bytes).map_err(|_| FfiError::InvalidAddress { len: bytes.len() })
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

/// Extract the chunk provider from the built client's providers.
fn chunks_from(providers: ClientRpcProviders<Arc<Identity>, ClientChunks>) -> ClientChunks {
    providers.chunks().clone()
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
        let stamped = reconstruct_upload(&upload(wire, address.clone(), vec![0u8; 113])).unwrap();
        assert_eq!(stamped.address().as_bytes(), address.as_slice());
        assert!(stamped.chunk().is_content());
    }

    #[test]
    fn reconstruct_rejects_short_address() {
        let (wire, _) = content_wire(b"payload");
        let err = reconstruct_upload(&upload(wire, vec![0u8; 4], vec![0u8; 113])).unwrap_err();
        assert!(matches!(err, FfiError::InvalidAddress { len: 4 }));
    }

    #[test]
    fn reconstruct_rejects_short_stamp() {
        let (wire, address) = content_wire(b"payload");
        let err = reconstruct_upload(&upload(wire, address, vec![0u8; 10])).unwrap_err();
        assert!(matches!(err, FfiError::InvalidStamp { .. }));
    }

    #[test]
    fn reconstruct_rejects_address_mismatch() {
        let (wire, _) = content_wire(b"payload");
        let wrong = vec![0xabu8; 32];
        let err = reconstruct_upload(&upload(wire, wrong, vec![0u8; 113])).unwrap_err();
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
