//! Browser download + manifest-walk flow.
//!
//! Manifests are read against the in-memory cache with a network prefetch-retry
//! loop. File reassembly drives the joiner directly over the network getter: its
//! pipelined in-flight pool fetches the chunk tree at the configured width and
//! reorders for delivery, so no separate prefetch pass is needed.

use std::sync::Arc;

use futures::StreamExt;
use js_sys::Uint8Array;
use nectar_mantaray::error::MantarayError;
use nectar_mantaray::{Entry, PlainManifest};
use nectar_primitives::file::WriteAt;
use nectar_primitives::store::ChunkStoreError;
use nectar_primitives::{ChunkAddress, DEFAULT_BODY_SIZE, Joiner};
use vertex_swarm_api::SwarmChunkProvider;
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::*;

use super::cache::MemoryCache;
use super::net_get::NetworkChunkGet;

#[wasm_bindgen]
extern "C" {
    /// A browser download sink (see `assets/download-sink.js`): an OPFS-worker
    /// sink that accepts out-of-order positional writes, or an ordered sink
    /// (File System Access or a service worker). `seekable` distinguishes them.
    #[wasm_bindgen(js_name = DownloadSink)]
    pub type DownloadSink;

    /// Announce the total byte count once the joiner is open (drives progress).
    #[wasm_bindgen(method, js_name = setTotal)]
    fn set_total(this: &DownloadSink, total: f64);

    /// Whether the sink accepts positional `write_at`, so leaves may be written
    /// out of order; an ordered sink must instead be fed in file order.
    #[wasm_bindgen(method, getter)]
    fn seekable(this: &DownloadSink) -> bool;

    /// Write one ordered segment; resolves when the sink can accept more.
    #[wasm_bindgen(method, catch)]
    async fn write(this: &DownloadSink, chunk: Uint8Array) -> Result<JsValue, JsValue>;

    /// Pre-size a seekable sink so every in-range `write_at` lands.
    #[wasm_bindgen(method, catch, js_name = setLen)]
    async fn set_len(this: &DownloadSink, len: f64) -> Result<JsValue, JsValue>;

    /// Write `chunk` at absolute byte `offset` (seekable sinks only).
    #[wasm_bindgen(method, catch, js_name = writeAt)]
    async fn write_at(
        this: &DownloadSink,
        offset: f64,
        chunk: Uint8Array,
    ) -> Result<JsValue, JsValue>;

    /// Finish the download, flushing and closing the underlying stream.
    #[wasm_bindgen(method, catch)]
    async fn close(this: &DownloadSink) -> Result<JsValue, JsValue>;

    /// Cancel the download with a human-readable reason.
    #[wasm_bindgen(method)]
    fn abort(this: &DownloadSink, reason: &str);
}

/// Error from a browser sink operation, carrying the JS message as a string.
#[derive(Debug)]
struct SinkError(String);

impl std::fmt::Display for SinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SinkError {}

/// Adapts a seekable browser [`DownloadSink`] as a nectar [`WriteAt`], so the
/// joiner writes each leaf at its offset out of order, straight to the OPFS
/// worker. Wasm-only: the sink futures are `!Send`, which the demo never needs.
struct JsWriteSink<'a> {
    sink: &'a DownloadSink,
}

impl WriteAt for JsWriteSink<'_> {
    type Error = SinkError;

    async fn write_at(&self, offset: u64, buf: &[u8]) -> Result<(), SinkError> {
        let view = Uint8Array::from(buf);
        self.sink
            .write_at(offset as f64, view)
            .await
            .map(|_| ())
            .map_err(|e| SinkError(format!("write_at: {e:?}")))
    }

    async fn set_len(&self, len: u64) -> Result<(), SinkError> {
        self.sink
            .set_len(len as f64)
            .await
            .map(|_| ())
            .map_err(|e| SinkError(format!("set_len: {e:?}")))
    }
}

/// Maximum prefetch round trips before giving up on a manifest op.
const MAX_PREFETCH_ITERS: usize = 4096;

/// Chunk retrievals kept in flight while the joiner walks a file's chunk tree.
const DOWNLOAD_CONCURRENCY: usize = 32;

/// Leaf bodies held at once while streaming to a sequential sink: in-flight plus
/// buffered-for-reorder. At least `2 * DOWNLOAD_CONCURRENCY` keeps the pool full
/// even when the lowest-offset leaf is the straggler the sink is waiting on.
const STREAM_WINDOW: usize = DOWNLOAD_CONCURRENCY * 2;

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

/// Stream the file at `file_root` to `sink`, driving the joiner over the network
/// getter at full width. A seekable sink (the OPFS worker) takes positional
/// writes so leaves land out of order as they resolve; an ordered sink takes a
/// windowed in-order stream. Either way peak wasm buffering is bounded by the
/// in-flight width, not the file size.
pub async fn stream_file(
    file_root: ChunkAddress,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
    sink: &DownloadSink,
) -> Result<(), JsValue> {
    let getter = NetworkChunkGet::new(provider, cache.snapshot_map());
    let joiner = Joiner::<NetworkChunkGet, DEFAULT_BODY_SIZE>::new(getter, file_root)
        .await
        .map_err(|e| JsValue::from_str(&format!("joiner open: {e}")))?
        .with_concurrency(DOWNLOAD_CONCURRENCY);

    // Total is known once the joiner is open; announce it before streaming so
    // the progress bar can show a fraction rather than a bare byte count.
    sink.set_total(joiner.size() as f64);

    if joiner.size() == 0 {
        return finish(sink).await;
    }

    if sink.seekable() {
        // Out-of-order positional writes: the joiner writes each leaf at its
        // offset the moment it resolves, off the fetch thread, so a slow chunk
        // never gates the writes already in hand.
        if let Err(e) = joiner.download_into(JsWriteSink { sink }).await {
            sink.abort(&format!("download: {e}"));
            return Err(JsValue::from_str(&format!("download: {e}")));
        }
        return finish(sink).await;
    }

    // Ordered fallback: a sequential disk stream (File System Access append or a
    // service-worker download body) must be fed in file order. The windowed
    // reader fetches the tree at full width but reorders to in-order emission,
    // bounding held leaf bodies to the window.
    let mut reader = joiner.into_windowed_reader(STREAM_WINDOW);
    let stream = reader.stream();
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
        match manifest.entries().await {
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

/// Reassemble the file at `file_root` into memory, driving the joiner over the
/// network getter. Leaves are fetched at the configured width and written into a
/// pre-sized buffer at their offsets as they land, so arrival order does not gate
/// assembly. Returns the whole file in memory; callers wanting bounded memory for
/// large files stream to a sink via [`stream_file`] instead.
pub async fn download_file(
    file_root: ChunkAddress,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
) -> Result<Vec<u8>, JsValue> {
    let getter = NetworkChunkGet::new(provider, cache.snapshot_map());
    let joiner = Joiner::<NetworkChunkGet, DEFAULT_BODY_SIZE>::new(getter, file_root)
        .await
        .map_err(|e| JsValue::from_str(&format!("joiner open: {e}")))?
        .with_concurrency(DOWNLOAD_CONCURRENCY);

    let size = joiner.size() as usize;
    if size == 0 {
        return Ok(Vec::new());
    }

    let mut buf = vec![0u8; size];
    let stream = joiner.into_offset_stream_chunked();
    futures::pin_mut!(stream);
    while let Some(item) = stream.next().await {
        let (offset, body) = item.map_err(|e| JsValue::from_str(&format!("joiner read: {e}")))?;
        let start = offset as usize;
        buf[start..start + body.len()].copy_from_slice(&body);
    }
    Ok(buf)
}

/// List the manifest at `root` as `(path, address_hex)` pairs.
pub async fn ls_manifest(
    root: ChunkAddress,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
) -> Result<Vec<(String, String)>, JsValue> {
    let entries = prefetch_then(provider, cache, |c| async move {
        let mut manifest: PlainManifest<MemoryCache> = PlainManifest::open(root, c);
        manifest.entries().await
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
        let path = path_owned.clone();
        async move {
            let mut manifest: PlainManifest<MemoryCache> = PlainManifest::open(root, c);
            manifest.lookup(&path).await
        }
    })
    .await?;

    let file_root = entry
        .address()
        .copied()
        .ok_or_else(|| JsValue::from_str(&format!("manifest entry '{path}' has no reference")))?;

    download_file(file_root, provider, cache).await
}

/// Run a mantaray op against the cache, fetching missing chunks and retrying.
async fn prefetch_then<T, F, Fut>(
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
    mut op: F,
) -> Result<T, JsValue>
where
    F: FnMut(MemoryCache) -> Fut,
    Fut: std::future::Future<Output = Result<T, MantarayError>>,
{
    for _ in 0..MAX_PREFETCH_ITERS {
        match op(cache.clone()).await {
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
    let ChunkStoreError::NotFound(address) = store_err else {
        return None;
    };
    Some(*address)
}
