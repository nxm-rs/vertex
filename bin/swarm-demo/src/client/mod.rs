//! Browser Swarm client surface (file upload/download, mantaray walk, batch
//! discovery), exposed to JS via wasm-bindgen.

mod cache;
mod chain;
mod download;
mod net_get;
mod network;
mod upload;
mod usage;

use std::sync::Arc;

use alloy_signer_local::PrivateKeySigner;
use nectar_postage::Batch;
use nectar_primitives::ChunkAddress;
use vertex_swarm_api::{SwarmChunkProvider, SwarmChunkSender};
use vertex_swarm_node::LaunchedClient;
use wasm_bindgen::prelude::*;

use cache::MemoryCache;
use network::BrowserChunkProvider;

/// Fallback batch geometry, used only when on-chain discovery is unavailable.
const DEFAULT_BATCH_DEPTH: u8 = 20;
const DEFAULT_BUCKET_DEPTH: u8 = 16;

/// The browser file client over the launched node's provider/sender and a
/// session-local chunk cache.
#[wasm_bindgen]
#[derive(Clone)]
pub struct SwarmClient {
    provider: Arc<dyn SwarmChunkProvider>,
    sender: Arc<dyn SwarmChunkSender>,
    cache: MemoryCache,
}

impl SwarmClient {
    /// Build the client surface over an already-launched browser node.
    pub fn from_launched(launched: &LaunchedClient) -> Self {
        // The launched client's handle already paces its own outbound retrieval
        // and pushsync under each peer's pseudosettle allowance (the builder
        // wires the self-throttle), so the provider reuses it as-is.
        let client = launched.client().clone();
        let routing = BrowserChunkProvider::new(client, launched.topology().clone());
        let provider: Arc<dyn SwarmChunkProvider> = Arc::new(routing.clone());
        let sender: Arc<dyn SwarmChunkSender> = Arc::new(routing);
        Self {
            provider,
            sender,
            cache: MemoryCache::new(),
        }
    }
}

#[wasm_bindgen]
impl SwarmClient {
    /// Upload `bytes` as a file, returning the mantaray manifest root as hex.
    /// An optional `rpc_url` recovers the batch's real on-chain geometry.
    #[wasm_bindgen(js_name = uploadFile)]
    pub async fn upload_file(
        &self,
        bytes: Vec<u8>,
        filename: String,
        batch_id_hex: String,
        owner_key_hex: String,
        rpc_url: String,
        from_block: u64,
    ) -> Result<String, JsValue> {
        let signer = parse_signer(&owner_key_hex)?;
        let batch_id = parse_b256(&batch_id_hex)?;
        let owner = signer.address();

        // Task B follow-up #5: resolve the real batch geometry from the
        // discoverBatches path rather than assuming defaults. Only fall back to
        // defaults when discovery is unavailable (no rpc) or the batch is not
        // found in the queried window, and warn so the operator knows.
        let batch = if rpc_url.is_empty() {
            tracing::warn!(
                "uploadFile: no rpc_url provided; using default batch geometry \
                 (depth {DEFAULT_BATCH_DEPTH}, bucket {DEFAULT_BUCKET_DEPTH}). Pass an rpc_url \
                 to recover the real on-chain geometry."
            );
            default_batch(batch_id, owner)
        } else {
            let from = if from_block == 0 {
                chain::POSTAGE_STAMP_DEPLOY_BLOCK
            } else {
                from_block
            };
            match chain::resolve_batch(batch_id, owner, &rpc_url, from, None).await {
                Ok(Some(batch)) => batch,
                Ok(None) => {
                    tracing::warn!(
                        "uploadFile: batch {batch_id_hex} not found on-chain in the queried \
                         window; using default geometry (depth {DEFAULT_BATCH_DEPTH}, bucket \
                         {DEFAULT_BUCKET_DEPTH})."
                    );
                    default_batch(batch_id, owner)
                }
                Err(e) => {
                    tracing::warn!(
                        ?e,
                        "uploadFile: batch discovery failed; using default geometry \
                         (depth {DEFAULT_BATCH_DEPTH}, bucket {DEFAULT_BUCKET_DEPTH})"
                    );
                    default_batch(batch_id, owner)
                }
            }
        };

        let root = upload::upload_file(
            &bytes,
            &filename,
            &batch,
            signer,
            self.provider.clone(),
            self.sender.clone(),
            &self.cache,
        )
        .await?;

        Ok(root.to_string())
    }

    /// Reassemble the file referenced by `reference_hex` (a file root) into bytes.
    #[wasm_bindgen(js_name = downloadFile)]
    pub async fn download_file(&self, reference_hex: String) -> Result<Vec<u8>, JsValue> {
        let root = parse_address(&reference_hex)?;
        download::download_reference(root, self.provider.clone(), &self.cache).await
    }

    /// List the entries of the manifest rooted at `root_hex` (JS `{ path, address }`).
    #[wasm_bindgen(js_name = lsManifest)]
    pub async fn ls_manifest(&self, root_hex: String) -> Result<js_sys::Array, JsValue> {
        let root = parse_address(&root_hex)?;
        let entries = download::ls_manifest(root, self.provider.clone(), &self.cache).await?;
        let out = js_sys::Array::new();
        for (path, address) in entries {
            let obj = js_sys::Object::new();
            let _ =
                js_sys::Reflect::set(&obj, &JsValue::from_str("path"), &JsValue::from_str(&path));
            let _ = js_sys::Reflect::set(
                &obj,
                &JsValue::from_str("address"),
                &JsValue::from_str(&address),
            );
            out.push(&obj);
        }
        Ok(out)
    }

    /// Resolve `path` in the manifest rooted at `root_hex` to the file's bytes.
    #[wasm_bindgen]
    pub async fn walk(&self, root_hex: String, path: String) -> Result<Vec<u8>, JsValue> {
        let root = parse_address(&root_hex)?;
        download::walk(root, &path, self.provider.clone(), &self.cache).await
    }

    /// Discover batches owned by `owner_key_hex` from `BatchCreated` logs
    /// (`to_block` 0 means latest); returns a JS array of batch objects.
    #[wasm_bindgen(js_name = discoverBatches)]
    pub async fn discover_batches(
        &self,
        owner_key_hex: String,
        rpc_url: String,
        from_block: u64,
        to_block: u64,
    ) -> Result<js_sys::Array, JsValue> {
        let signer = parse_signer(&owner_key_hex)?;
        let owner = signer.address();
        let to = (to_block != 0).then_some(to_block);
        let batches = chain::discover_batches(owner, &rpc_url, from_block, to).await?;

        let out = js_sys::Array::new();
        for b in batches {
            let obj = js_sys::Object::new();
            set_str(&obj, "batchId", &format!("0x{}", hex::encode(b.batch_id)));
            set_str(&obj, "owner", &b.owner.to_string());
            set_num(&obj, "depth", b.depth as f64);
            set_num(&obj, "bucketDepth", b.bucket_depth as f64);
            let _ = js_sys::Reflect::set(
                &obj,
                &JsValue::from_str("immutable"),
                &JsValue::from_bool(b.immutable),
            );
            set_str(&obj, "normalisedBalance", &b.normalised_balance.to_string());
            out.push(&obj);
        }
        Ok(out)
    }

    /// The mainnet PostageStamp deployment block, a `from_block` floor for discovery.
    #[wasm_bindgen(js_name = postageDeployBlock)]
    pub fn postage_deploy_block() -> u64 {
        chain::POSTAGE_STAMP_DEPLOY_BLOCK
    }
}

/// Build an immutable [`Batch`] from the default geometry (discovery fallback).
fn default_batch(batch_id: alloy_primitives::B256, owner: alloy_primitives::Address) -> Batch {
    Batch::new(
        batch_id,
        0,
        0,
        owner,
        DEFAULT_BATCH_DEPTH,
        DEFAULT_BUCKET_DEPTH,
        true,
    )
}

fn set_str(obj: &js_sys::Object, key: &str, value: &str) {
    let _ = js_sys::Reflect::set(obj, &JsValue::from_str(key), &JsValue::from_str(value));
}

fn set_num(obj: &js_sys::Object, key: &str, value: f64) {
    let _ = js_sys::Reflect::set(obj, &JsValue::from_str(key), &JsValue::from_f64(value));
}

/// Parse a 32-byte hex private key into a signer.
fn parse_signer(key_hex: &str) -> Result<PrivateKeySigner, JsValue> {
    let bytes = parse_hex_32(key_hex)?;
    PrivateKeySigner::from_bytes(&bytes.into())
        .map_err(|e| JsValue::from_str(&format!("invalid owner key: {e}")))
}

/// Parse a 32-byte hex value into a `B256`.
fn parse_b256(s: &str) -> Result<alloy_primitives::B256, JsValue> {
    Ok(alloy_primitives::B256::from(parse_hex_32(s)?))
}

/// Parse a 32-byte hex value into a `ChunkAddress`.
fn parse_address(s: &str) -> Result<ChunkAddress, JsValue> {
    Ok(ChunkAddress::new(parse_hex_32(s)?))
}

/// Parse a (possibly `0x`-prefixed) 32-byte hex string.
fn parse_hex_32(s: &str) -> Result<[u8; 32], JsValue> {
    let trimmed = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(trimmed).map_err(|e| JsValue::from_str(&format!("bad hex: {e}")))?;
    if bytes.len() != 32 {
        return Err(JsValue::from_str(&format!(
            "expected 32 bytes, got {}",
            bytes.len()
        )));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(arr)
}
