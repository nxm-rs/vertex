//! Browser upload flow: split -> stamp -> push -> manifest -> persist usage,
//! returning the mantaray manifest root.

use std::sync::Arc;

use alloy_signer_local::PrivateKeySigner;
use nectar_mantaray::PlainManifest;
use nectar_postage::Batch;
use nectar_postage_issuer::{BatchStamper as IssuerStamper, Stamper};
use nectar_postage_usage::SnapshotIssuer;
use nectar_primitives::file::sync_split;
use nectar_primitives::{AnyChunk, ChunkAddress, DEFAULT_BODY_SIZE};
use vertex_swarm_api::{StampedChunk, SwarmChunkProvider, SwarmChunkSender};
use vertex_swarm_primitives::Stamp;
use wasm_bindgen::JsValue;

use super::cache::MemoryCache;
use super::usage::{BrowserUsageSink, BrowserUsageSource, flush_snapshot, open_snapshot};

/// Maximum concurrent pushes (the pipeline bounds by chunk count).
const PUSH_CONCURRENCY: u32 = 8;

/// Upload `bytes` as a single named file, returning the mantaray manifest root.
pub async fn upload_file(
    bytes: &[u8],
    filename: &str,
    batch: &Batch,
    signer: PrivateKeySigner,
    provider: Arc<dyn SwarmChunkProvider>,
    sender: Arc<dyn SwarmChunkSender>,
    cache: &MemoryCache,
) -> Result<ChunkAddress, JsValue> {
    let owner = signer.address();

    // Usage transport adapters over the routing provider/sender. Used to recover
    // the snapshot from Swarm (cross-device) and to persist it back.
    let source = BrowserUsageSource::new(provider);
    let sink = BrowserUsageSink::new(sender.clone());

    // 1. Split the file into content chunks. rayon runs inline on wasm
    //    (no `wasm-threads`), so this is single-threaded.
    let (file_root, store) =
        sync_split::<DEFAULT_BODY_SIZE>(bytes).map_err(|e| js_err("split", e))?;

    // Move content chunks into the session cache so they are immediately
    // retrievable and so the manifest save can reference the file root.
    for (_, chunk) in store.into_chunks() {
        cache.insert(chunk);
    }

    // 2. Build and save the manifest. Its nodes are written into the cache.
    let mut manifest: PlainManifest<MemoryCache> = PlainManifest::new(cache.clone());
    manifest
        .add(filename, file_root)
        .map_err(|e| js_err("manifest add", e))?;
    let manifest_root = manifest.save().map_err(|e| js_err("manifest save", e))?;

    // 3. Stamp every chunk in the cache (content + manifest nodes) and push.
    //    Recover the usage snapshot from Swarm (root + committed leaves) so this
    //    upload accumulates prior usage (from another client or a previous
    //    browser session) and respects the published-sequence floor. A
    //    `SnapshotIssuer` over the recovered snapshot feeds the issuer stamper,
    //    which signs each stamp with the owner key.
    let snapshot = open_snapshot(&source, batch).await?;
    let issuer = SnapshotIssuer::new(snapshot, owner);
    let mut stamper = IssuerStamper::new(issuer, signer.clone());

    let mut stamped: Vec<StampedChunk> = Vec::new();
    // Collect addresses first to avoid holding the cache borrow across stamping.
    let chunks: Vec<AnyChunk> = cache_chunks(cache);
    for chunk in chunks {
        let address = *chunk.address();
        let stamp: Stamp = stamper
            .stamp(&address)
            .map_err(|e| js_err("content stamp", e))?;
        stamped.push(StampedChunk::new(chunk, stamp));
    }

    push_all(&sender, stamped).await?;

    // 4. Persist batch usage as single-owner chunks and push them too. Recover
    //    the snapshot through the issuer, re-read the live published floor, plan,
    //    seal, and upload. This is what makes the usage roam across devices and
    //    honours the anti-downgrade floor.
    let snapshot = stamper.issuer_mut().snapshot_mut();
    flush_snapshot(snapshot, &owner, &signer, &source, &sink).await?;

    Ok(manifest_root)
}

/// Snapshot the cache's chunks into an owned vec.
fn cache_chunks(cache: &MemoryCache) -> Vec<AnyChunk> {
    // MemoryCache holds an `Rc<RefCell<HashMap>>`; clone the values out.
    let mut out = Vec::with_capacity(cache.len());
    cache.for_each(|chunk| out.push(chunk.clone()));
    out
}

/// Drive the push combinator over `chunks`, surfacing the first per-chunk error.
async fn push_all(
    sender: &Arc<dyn SwarmChunkSender>,
    chunks: Vec<StampedChunk>,
) -> Result<(), JsValue> {
    use futures::StreamExt;

    if chunks.is_empty() {
        return Ok(());
    }
    let config = vertex_swarm_stream::StreamConfig::new(PUSH_CONCURRENCY as usize);
    let mut stream = Box::pin(vertex_swarm_stream::put_stream(
        sender.clone(),
        chunks,
        config,
    ));
    // Each item carries its address, so a failed push names the right chunk.
    while let Some((address, result)) = stream.next().await {
        if let Err(e) = result {
            return Err(JsValue::from_str(&format!(
                "push failed for chunk {address}: {e}"
            )));
        }
    }
    Ok(())
}

fn js_err(stage: &str, e: impl core::fmt::Display) -> JsValue {
    JsValue::from_str(&format!("{stage}: {e}"))
}
