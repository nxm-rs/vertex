//! Headless worker-node entry: boots a client node inside a Web Worker (no DOM,
//! no `window`) and exposes chunk retrieval over a wasm-bindgen handle.
//!
//! WebSocket connections are budgeted per worker, not per origin, so each worker
//! holds an independent peer set and an independent per-peer in-flight allowance.
//! Sharding retrieval across K workers therefore multiplies the forwarding fan a
//! single main-thread node is bounded by, with throughput scaling roughly with K.

use std::sync::Arc;

use tracing::info;
use vertex_net_dnsaddr_doh::{DohClient, resolve_mainnet_wss_bootnodes};
use vertex_swarm_api::{SwarmChunkProvider, SwarmIdentity};
use vertex_swarm_identity::Identity;
use vertex_swarm_node::{ClientLauncher, SwarmNodeType};
use vertex_swarm_spec::{init_mainnet, mainnet_wss_bootnodes};
use wasm_bindgen::prelude::*;

use crate::client::{BrowserChunkProvider, parse_address};

/// A node running inside a Web Worker, exposing chunk retrieval to the worker's
/// message handler. Holds the task manager so the spawned node tasks live for the
/// worker's lifetime.
#[wasm_bindgen]
pub struct WorkerNode {
    provider: Arc<dyn SwarmChunkProvider>,
    overlay: String,
    _task_manager: vertex_tasks::TaskManager,
}

#[wasm_bindgen]
impl WorkerNode {
    /// This node's overlay address as a hex string.
    #[wasm_bindgen(getter)]
    pub fn overlay(&self) -> String {
        self.overlay.clone()
    }

    /// Retrieve one chunk by address-hex, returning its raw chunk bytes (the wire
    /// chunk: span prefix plus body). The caller reassembles the tree.
    #[wasm_bindgen(js_name = fetchChunk)]
    pub async fn fetch_chunk(&self, address_hex: String) -> Result<Vec<u8>, JsValue> {
        let address = parse_address(&address_hex)?;
        let result = self
            .provider
            .retrieve_chunk(&address)
            .await
            .map_err(|e| JsValue::from_str(&format!("retrieve failed: {e}")))?;
        // `data()` is the full wire chunk: 8-byte little-endian span then body.
        Ok(result.chunk.data().to_vec())
    }

    /// Resolve the manifest at `reference_hex` to its file root, then walk the
    /// chunk tree returning every chunk address (intermediates and leaves) as a
    /// hex array. The shard coordinator partitions this list across workers.
    #[wasm_bindgen(js_name = listChunks)]
    pub async fn list_chunks(
        &self,
        reference_hex: String,
        max_addresses: usize,
    ) -> Result<js_sys::Array, JsValue> {
        let root = parse_address(&reference_hex)?;
        let cache = crate::client::MemoryCache::new();
        let addrs =
            crate::client::list_tree_addresses(root, self.provider.clone(), &cache, max_addresses)
                .await?;
        let out = js_sys::Array::new();
        for a in addrs {
            out.push(&JsValue::from_str(&a.to_string()));
        }
        Ok(out)
    }
}

/// Boot a headless client node inside a Web Worker.
///
/// No DOM, no UI, no `window`: the timer, clock, DoH `fetch`, and WebSocket
/// transport are all available in a `WorkerGlobalScope`, so the node
/// infrastructure runs unchanged.
///
/// # Errors
/// Returns a JS error if bootnode resolution or the node launch fails.
#[wasm_bindgen(js_name = startWorkerNode)]
pub async fn start_worker_node() -> Result<WorkerNode, JsValue> {
    console_error_panic_hook::set_once();
    crate::init_tracing();

    // The global executor must be installed before building the node: topology,
    // peer-manager tick, and the client service resolve their spawner through it.
    let task_manager = vertex_tasks::TaskManager::current();

    let spec = init_mainnet();
    let identity = Identity::random(spec, SwarmNodeType::Client);
    let overlay = identity.overlay_address().to_string();
    info!(%overlay, "worker node: resolving bootnodes");

    let bootnodes =
        resolve_mainnet_wss_bootnodes(&DohClient::cloudflare(), mainnet_wss_bootnodes()).await;
    info!(count = bootnodes.len(), "worker node: dialing bootnodes");

    let launched = ClientLauncher::new(identity)
        .with_bootnodes(bootnodes)
        .launch()
        .await
        .map_err(|e| JsValue::from_str(&format!("worker launch failed: {e}")))?;

    let provider: Arc<dyn SwarmChunkProvider> = Arc::new(BrowserChunkProvider::new(
        launched.client().clone(),
        launched.topology().clone(),
    ));

    Ok(WorkerNode {
        provider,
        overlay,
        _task_manager: task_manager,
    })
}
