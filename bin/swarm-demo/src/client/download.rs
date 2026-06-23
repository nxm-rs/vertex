//! Browser download + manifest-walk flow.
//!
//! Manifests are read against the in-memory cache with a network prefetch-retry
//! loop. Buffered reassembly (`download_file`) enumerates the chunk tree and
//! prefetches it breadth-first before assembling from the warm cache. The
//! streamed path (`stream_file`) prefetches the tree concurrently with the
//! joiner's ordered reads, so the joiner reads mostly cached chunks while the
//! fan-out stays per-peer-bounded by the client throttle.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use js_sys::Uint8Array;
use nectar_mantaray::error::MantarayError;
use nectar_mantaray::{Entry, PlainManifest};
use nectar_primitives::store::ChunkStoreError;
use nectar_primitives::{AnyChunk, ChunkAddress, DEFAULT_BODY_SIZE, Joiner};
use vertex_swarm_api::SwarmChunkProvider;
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::*;

use super::cache::MemoryCache;
use super::net_get::NetworkChunkGet;

#[wasm_bindgen]
extern "C" {
    /// A browser download sink (see `assets/download-sink.js`): ordered segment
    /// writes with backpressure, streamed to disk via the File System Access API
    /// or a service worker. The Rust side never inspects the chosen path.
    #[wasm_bindgen(js_name = DownloadSink)]
    pub type DownloadSink;

    /// Announce the total byte count once the joiner is open (drives progress).
    #[wasm_bindgen(method, js_name = setTotal)]
    fn set_total(this: &DownloadSink, total: f64);

    /// Write one ordered segment; resolves when the sink can accept more.
    #[wasm_bindgen(method, catch)]
    async fn write(this: &DownloadSink, chunk: Uint8Array) -> Result<JsValue, JsValue>;

    /// Finish the download, flushing and closing the underlying stream.
    #[wasm_bindgen(method, catch)]
    async fn close(this: &DownloadSink) -> Result<JsValue, JsValue>;

    /// Cancel the download with a human-readable reason.
    #[wasm_bindgen(method)]
    fn abort(this: &DownloadSink, reason: &str);
}

/// Bytes of a single child reference in a plain-mode intermediate node body.
const REF_SIZE: usize = 32;

/// Maximum prefetch round trips before giving up on a manifest op.
const MAX_PREFETCH_ITERS: usize = 4096;

/// Chunk retrievals the tree prefetch keeps in flight at once.
///
/// Both download paths prefetch concurrently: `download_file` before assembling,
/// `stream_file` alongside the ordered stream. The provider's per-peer in-flight
/// cap skips a saturated storer to the next-closest one, so a wide fan-out
/// spreads load across the neighbourhood instead of piling depth onto the closest
/// few. This is a depth-stability knob, not a throughput lever: download
/// throughput is bounded by per-chunk retrieval latency, which rises with this
/// width as in-flight requests queue, so aggregate throughput stays flat while a
/// wider fan-out holds the reserve depth steadier. Past a few hundred the fan-out
/// starts shedding peers; this width sits below that knee.
const PREFETCH_CONCURRENCY: usize = 128;

/// Chunk reads the joiner keeps in flight while assembling from the warm cache.
///
/// With the concurrent prefetch warming the shared map ahead of it, the joiner's
/// ordered reads mostly hit the cache, so this is a lookahead window over warm
/// chunks rather than the retrieval breadth. Per-storer pressure on any
/// network miss is still bounded by the provider's per-peer cap.
const JOIN_CONCURRENCY: usize = 64;

/// Download the file at `root`, resolving it as a single-file manifest if it is one.
pub async fn download_reference(
    root: ChunkAddress,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
) -> Result<Vec<u8>, JsValue> {
    match probe_manifest_entries(root, provider.clone(), cache).await? {
        // `root` is a manifest: pick the file entry to return.
        Some(entries) => {
            let file_root = pick_manifest_file(&entries)?;
            download_file(file_root, provider, cache).await
        }
        // `root` is a plain file content chunk: join it directly.
        None => download_file(root, provider, cache).await,
    }
}

/// Stream the file at `root` to a browser `sink`, resolving a single-file
/// manifest if `root` is one. Bytes flow to disk in order with backpressure;
/// no full copy of the file is held in wasm memory.
pub async fn stream_reference(
    root: ChunkAddress,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
    sink: &DownloadSink,
) -> Result<(), JsValue> {
    let file_root = match probe_manifest_entries(root, provider.clone(), cache).await? {
        Some(entries) => pick_manifest_file(&entries)?,
        None => root,
    };
    stream_file(file_root, provider, cache, sink).await
}

/// Stream the file at `file_root` to `sink` as ordered segments.
///
/// The joiner emits segments strictly in order, fetching one subtree at a time,
/// so driving retrieval from the joiner alone serialises the whole download to
/// one round trip per subtree. Instead a concurrent prefetch walks the chunk
/// tree breadth-first and warms the shared map the joiner reads from, so the
/// joiner's ordered reads hit the warm map at memory speed. The prefetch fans
/// out wide across the neighbourhood; the per-peer in-flight cap in the client
/// throttle is what keeps that fan-out from flooding any single storer. Each
/// streamed segment is dropped after its write resolves, bounding wasm
/// buffering to one segment in flight.
pub async fn stream_file(
    file_root: ChunkAddress,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
    sink: &DownloadSink,
) -> Result<(), JsValue> {
    let getter = NetworkChunkGet::new(provider, cache.snapshot_map());
    // The getter's live map, shared with the prefetch below so the joiner's
    // ordered reads find prefetched chunks instead of re-fetching them.
    let shared = getter.shared();
    let provider = getter.provider();
    let joiner = Joiner::<NetworkChunkGet, DEFAULT_BODY_SIZE>::new(getter, file_root)
        .await
        .map_err(|e| JsValue::from_str(&format!("joiner open: {e}")))?
        .with_concurrency(JOIN_CONCURRENCY);

    // Total is known once the joiner is open; announce it before streaming so
    // the progress bar can show a fraction rather than a bare byte count.
    sink.set_total(joiner.size() as f64);

    if joiner.size() == 0 {
        return finish(sink).await;
    }

    // Warm the getter's live map concurrently with the ordered stream: the
    // prefetch races ahead fetching the tree in parallel (fanned wide across
    // peers, bounded per-peer by the client throttle) while the joiner reads
    // mostly cached segments in order and writes them to the sink.
    let prefetch = prefetch_into_shared(file_root, provider, shared);
    let stream_out = async {
        let stream = joiner.into_stream();
        futures::pin_mut!(stream);
        while let Some(segment) = stream.next().await {
            let segment = match segment {
                Ok(seg) => seg,
                Err(e) => {
                    sink.abort(&format!("joiner read: {e}"));
                    return Err(JsValue::from_str(&format!("joiner read: {e}")));
                }
            };
            // Copy this segment into a JS view and write it; await applies the
            // sink's backpressure. The segment is dropped at the end of the loop
            // body, so peak wasm buffering is one segment plus its JS copy.
            let view = Uint8Array::from(&segment[..]);
            if let Err(e) = sink.write(view).await {
                sink.abort("write failed");
                return Err(JsValue::from_str(&format!("sink write: {e:?}")));
            }
        }
        Ok(())
    };

    // Run both to completion. A prefetch error is non-fatal on its own (the
    // joiner can still fetch the chunk itself), so only a stream error aborts.
    let (prefetch_result, stream_result) = futures::future::join(prefetch, stream_out).await;
    stream_result?;
    prefetch_result?;

    finish(sink).await
}

/// Close `sink`, mapping a close failure to a `JsValue` error.
async fn finish(sink: &DownloadSink) -> Result<(), JsValue> {
    sink.close()
        .await
        .map(|_| ())
        .map_err(|e| JsValue::from_str(&format!("sink close: {e:?}")))
}

/// Probe whether `root` is a manifest, returning its entries or `None` if not.
async fn probe_manifest_entries(
    root: ChunkAddress,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
) -> Result<Option<Vec<Entry>>, JsValue> {
    for _ in 0..MAX_PREFETCH_ITERS {
        let mut manifest: PlainManifest<MemoryCache> = PlainManifest::open(root, cache.clone());
        match manifest.entries() {
            Ok(entries) if !entries.is_empty() => return Ok(Some(entries)),
            // Parsed as a manifest but empty: treat as "not a usable manifest"
            // and fall through to a raw file join.
            Ok(_) => return Ok(None),
            Err(e) => match missing_address(&e) {
                // A missing chunk: fetch it and retry the probe.
                Some(missing) => {
                    let result = provider
                        .retrieve_chunk(&missing)
                        .await
                        .map_err(|e| JsValue::from_str(&format!("retrieve {missing}: {e}")))?;
                    cache.insert(result.chunk);
                }
                // Any other parse error means the root chunk is a plain file
                // content chunk, not a mantaray node: not a manifest.
                None => return Ok(None),
            },
        }
    }
    Err(JsValue::from_str(
        "manifest probe exceeded prefetch budget; chunk likely unavailable",
    ))
}

/// Pick the file root from a manifest's `entries`, preferring `index.html` or `/`.
fn pick_manifest_file(entries: &[Entry]) -> Result<ChunkAddress, JsValue> {
    let valued: Vec<&Entry> = entries.iter().filter(|e| e.address().is_some()).collect();

    match valued.as_slice() {
        [] => Err(JsValue::from_str(
            "reference is a manifest with no value-bearing entries",
        )),
        [only] => Ok(*only.address().expect("filtered to value-bearing")),
        many => {
            if let Some(preferred) = many
                .iter()
                .find(|e| matches!(e.path_str(), Some("index.html") | Some("/")))
            {
                return Ok(*preferred.address().expect("filtered to value-bearing"));
            }
            let paths: Vec<&str> = many
                .iter()
                .map(|e| e.path_str().unwrap_or("<non-utf8>"))
                .collect();
            Err(JsValue::from_str(&format!(
                "reference is a manifest with {} files; download a specific path via walk(). entries: {}",
                many.len(),
                paths.join(", ")
            )))
        }
    }
}

/// Reassemble the file at `file_root`: prefetch its chunk tree, then join from cache.
pub async fn download_file(
    file_root: ChunkAddress,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
) -> Result<Vec<u8>, JsValue> {
    prefetch_tree(file_root, provider.clone(), cache).await?;

    let getter = NetworkChunkGet::new(provider, cache.snapshot_map());
    join_to_bytes(file_root, getter).await
}

/// List the manifest at `root` as `(path, address_hex)` pairs.
pub async fn ls_manifest(
    root: ChunkAddress,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
) -> Result<Vec<(String, String)>, JsValue> {
    let entries = prefetch_then(provider, cache, |c| {
        let mut manifest: PlainManifest<MemoryCache> = PlainManifest::open(root, c.clone());
        manifest.entries()
    })
    .await?;

    Ok(entries
        .iter()
        .map(|e| {
            let path = e.path_str().unwrap_or("<non-utf8>").to_string();
            let addr = e
                .address()
                .map(|a| a.to_string())
                .unwrap_or_else(|| "<none>".to_string());
            (path, addr)
        })
        .collect())
}

/// Walk `path` in the manifest at `root`, returning the referenced file's bytes.
pub async fn walk(
    root: ChunkAddress,
    path: &str,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
) -> Result<Vec<u8>, JsValue> {
    let path_owned = path.to_string();
    let entry: Entry = prefetch_then(provider.clone(), cache, |c| {
        let mut manifest: PlainManifest<MemoryCache> = PlainManifest::open(root, c.clone());
        manifest.lookup(&path_owned)
    })
    .await?;

    let file_root = entry
        .address()
        .copied()
        .ok_or_else(|| JsValue::from_str(&format!("manifest entry '{path}' has no reference")))?;

    download_file(file_root, provider, cache).await
}

/// Prefetch the chunk tree at `root` into `cache`, breadth-first and concurrent.
async fn prefetch_tree(
    root: ChunkAddress,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
) -> Result<(), JsValue> {
    // Addresses whose chunk we have already fetched (or queued to fetch) this
    // pass: dedups shared subtrees and guards against a malformed cycle.
    let mut seen: HashSet<ChunkAddress> = HashSet::new();
    // The current level to fetch concurrently; starts with just the root.
    let mut level: Vec<ChunkAddress> = vec![root];
    seen.insert(root);

    while !level.is_empty() {
        // Fetch this whole level concurrently, skipping chunks already cached.
        let fetched: Vec<Result<AnyChunk, JsValue>> = futures::stream::iter(level.into_iter())
            .map(|addr| {
                let provider = Arc::clone(&provider);
                let cached = cache.fetch(&addr);
                async move {
                    match cached {
                        Some(chunk) => Ok(chunk),
                        None => provider
                            .retrieve_chunk(&addr)
                            .await
                            .map(|r| r.chunk)
                            .map_err(|e| JsValue::from_str(&format!("retrieve {addr}: {e}"))),
                    }
                }
            })
            .buffer_unordered(PREFETCH_CONCURRENCY)
            .collect()
            .await;

        // Insert the fetched chunks and gather the next level (children of the
        // intermediate nodes). A retrieval error here fails the download.
        let mut next: Vec<ChunkAddress> = Vec::new();
        for result in fetched {
            let chunk = result?;
            // Intermediate node ⇒ its body is packed child references; a leaf's
            // span fits one chunk body and ends the branch.
            if chunk.span() > DEFAULT_BODY_SIZE as u64 {
                for child in parse_child_refs(chunk.data())? {
                    if seen.insert(child) {
                        next.push(child);
                    }
                }
            }
            cache.insert(chunk);
        }
        level = next;
    }

    Ok(())
}

/// Prefetch the chunk tree at `root` into the joiner's live `shared` map,
/// breadth-first and concurrent.
///
/// Like [`prefetch_tree`] but writes into the getter's `Arc<Mutex<_>>` rather
/// than the `MemoryCache`, so the joiner streaming the same getter sees each
/// chunk the moment it lands and never re-fetches it.
async fn prefetch_into_shared(
    root: ChunkAddress,
    provider: Arc<dyn SwarmChunkProvider>,
    shared: Arc<Mutex<HashMap<ChunkAddress, AnyChunk>>>,
) -> Result<(), JsValue> {
    let mut seen: HashSet<ChunkAddress> = HashSet::new();
    let mut level: Vec<ChunkAddress> = vec![root];
    seen.insert(root);

    while !level.is_empty() {
        let fetched: Vec<Result<AnyChunk, JsValue>> = futures::stream::iter(level.into_iter())
            .map(|addr| {
                let provider = Arc::clone(&provider);
                let cached = shared.lock().expect("cache mutex").get(&addr).cloned();
                async move {
                    match cached {
                        Some(chunk) => Ok(chunk),
                        None => provider
                            .retrieve_chunk(&addr)
                            .await
                            .map(|r| r.chunk)
                            .map_err(|e| JsValue::from_str(&format!("retrieve {addr}: {e}"))),
                    }
                }
            })
            .buffer_unordered(PREFETCH_CONCURRENCY)
            .collect()
            .await;

        let mut next: Vec<ChunkAddress> = Vec::new();
        for result in fetched {
            let chunk = result?;
            if chunk.span() > DEFAULT_BODY_SIZE as u64 {
                for child in parse_child_refs(chunk.data())? {
                    if seen.insert(child) {
                        next.push(child);
                    }
                }
            }
            shared
                .lock()
                .expect("cache mutex")
                .insert(*chunk.address(), chunk);
        }
        level = next;
    }

    Ok(())
}

/// Parse an intermediate node's body as packed 32-byte child chunk addresses.
fn parse_child_refs(body: &[u8]) -> Result<Vec<ChunkAddress>, JsValue> {
    if body.len() % REF_SIZE != 0 {
        return Err(JsValue::from_str(&format!(
            "malformed intermediate node: body length {} is not a multiple of {REF_SIZE}",
            body.len()
        )));
    }
    Ok(body
        .chunks_exact(REF_SIZE)
        .map(|r| {
            let mut arr = [0u8; REF_SIZE];
            arr.copy_from_slice(r);
            ChunkAddress::new(arr)
        })
        .collect())
}

/// Reassemble the file at `root` from the warm cache-backed `getter`.
async fn join_to_bytes(root: ChunkAddress, getter: NetworkChunkGet) -> Result<Vec<u8>, JsValue> {
    let joiner = Joiner::<NetworkChunkGet, DEFAULT_BODY_SIZE>::new(getter, root)
        .await
        .map_err(|e| JsValue::from_str(&format!("joiner open: {e}")))?
        .with_concurrency(JOIN_CONCURRENCY);

    if joiner.size() == 0 {
        return Ok(Vec::new());
    }

    joiner
        .read_all()
        .await
        .map_err(|e| JsValue::from_str(&format!("joiner read: {e}")))
}

/// Run a mantaray op against the cache, fetching missing chunks and retrying.
async fn prefetch_then<T, F>(
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
    mut op: F,
) -> Result<T, JsValue>
where
    F: FnMut(&MemoryCache) -> Result<T, MantarayError>,
{
    for _ in 0..MAX_PREFETCH_ITERS {
        match op(cache) {
            Ok(value) => return Ok(value),
            Err(e) => {
                let missing = missing_address(&e).ok_or_else(|| {
                    JsValue::from_str(&format!("manifest op failed (not a missing chunk): {e}"))
                })?;
                // Fetch the missing chunk over the network and cache it, then
                // retry the whole operation (mantaray will get further this time).
                let result = provider
                    .retrieve_chunk(&missing)
                    .await
                    .map_err(|e| JsValue::from_str(&format!("retrieve {missing}: {e}")))?;
                cache.insert(result.chunk);
            }
        }
    }
    Err(JsValue::from_str(
        "manifest walk exceeded prefetch budget; chunk likely unavailable",
    ))
}

/// If `err` is a store-get miss, extract the missing chunk address.
fn missing_address(err: &MantarayError) -> Option<ChunkAddress> {
    let MantarayError::StoreGet { source } = err else {
        return None;
    };
    let store_err = source.downcast_ref::<ChunkStoreError>()?;
    let ChunkStoreError::NotFound { address_hex } = store_err else {
        return None;
    };
    parse_address_hex(address_hex)
}

/// Parse a (possibly `0x`-prefixed) 32-byte hex address.
fn parse_address_hex(s: &str) -> Option<ChunkAddress> {
    let trimmed = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(trimmed).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Some(ChunkAddress::new(arr))
}
