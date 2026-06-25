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
use super::net_get::{NetworkChunkGet, RetryingChunkGet};

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

    /// A random-access browser download sink (see `assets/random-access-sink.js`):
    /// positional writes that may arrive out of order, streamed to disk via a
    /// File System Access writable or an OPFS-staged file. The Rust side never
    /// inspects the chosen back-end.
    #[wasm_bindgen(js_name = RandomAccessSink)]
    pub type RandomAccessSink;

    /// Announce the total byte count once the root span is known (drives progress
    /// and lets the sink size the file up front).
    #[wasm_bindgen(method, js_name = setTotal)]
    fn set_total_ra(this: &RandomAccessSink, total: f64);

    /// Write `data` at byte `offset`; resolves when the sink can accept more.
    /// Offsets may be written in any order. `data` is backed by a fresh
    /// `ArrayBuffer` (copied out of wasm memory) so the sink may transfer it to
    /// the OPFS sink worker zero-copy; resolution awaits the worker's write ack,
    /// which is the backpressure signal.
    #[wasm_bindgen(method, catch, js_name = writeAt)]
    async fn write_at(
        this: &RandomAccessSink,
        offset: f64,
        data: Uint8Array,
    ) -> Result<JsValue, JsValue>;

    /// Finish the download, flushing and closing the underlying stream.
    #[wasm_bindgen(method, catch, js_name = close)]
    async fn close_ra(this: &RandomAccessSink) -> Result<JsValue, JsValue>;

    /// Cancel the download with a human-readable reason.
    #[wasm_bindgen(method, js_name = abort)]
    fn abort_ra(this: &RandomAccessSink, reason: &str);
}

/// Bytes of a single child reference in a plain-mode intermediate node body.
const REF_SIZE: usize = 32;

/// Maximum prefetch round trips before giving up on a manifest op.
const MAX_PREFETCH_ITERS: usize = 4096;

/// Chunk retrievals the tree prefetch keeps in flight at once.
///
/// Both download paths prefetch concurrently: `download_file` before assembling,
/// `stream_file` alongside the ordered stream. The provider's per-peer in-flight
/// cap skips a saturated storer to the next-closest one, so the fan-out spreads
/// load across the neighbourhood instead of piling depth onto the closest few,
/// which also holds the reserve depth steadier.
///
/// The effective ceiling is the distributed scheduler's admission pool, roughly
/// `connected-peers * per-peer-cap` slots, not connections, and a width past the
/// available slots bounces the surplus into busy re-picks. Measured on the live
/// network from the browser: a 512-wide fan-out admits retrievals faster than a
/// thin peer set can serve, so close storers reset under the in-flight load in a
/// synchronised wave (the surplus also lands on just-disconnected peers, so most
/// of it bounces as not-connected). 256 spreads the same work without
/// overrunning the serving capacity: against 512 it cut the in-download
/// transport-reset count by ~89% (median 28 to 3) and raised throughput ~36%
/// (median 241 to 327 KB/s), n=8 byte-verified each; 128 starves the pipeline
/// and throughput regresses. 256 is the operating point.
const DEFAULT_PREFETCH_CONCURRENCY: usize = 256;

static PREFETCH_CONCURRENCY_OVERRIDE: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(DEFAULT_PREFETCH_CONCURRENCY);

/// Override the prefetch fan-out width (`pf` URL param). Measurement aid so a
/// concurrency sweep needs no rebuild.
pub fn configure_prefetch(width: Option<usize>) {
    if let Some(w) = width.filter(|w| *w > 0) {
        PREFETCH_CONCURRENCY_OVERRIDE.store(w, std::sync::atomic::Ordering::Relaxed);
    }
}

fn prefetch_concurrency() -> usize {
    PREFETCH_CONCURRENCY_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed)
}

/// The configured prefetch fan-out width, for callers (the leaf-shard fetch)
/// that default their own width to the same operating point.
pub fn default_prefetch_width() -> usize {
    prefetch_concurrency()
}

/// Resolved retrievals between macrotask yields in a prefetch drain.
///
/// Each resolved leg cedes to the browser event loop through a `setTimeout(0)`
/// macrotask so the node run loop services the socket reads and timers that feed
/// the next responses; without it a wide fan-out monopolises the single thread
/// and the download wedges. Batching the yield (cede once every N resolutions)
/// was measured against the per-chunk yield on the live network: it is
/// throughput-neutral. At every N the prefetch pool already fills to its width
/// (~520 legs in flight across the connected peers at their per-peer cap), so the
/// resolve cadence is not the binding constraint; the per-leg latency is the
/// client throttle's pseudosettle-allowance pacing (the leg wall time is almost
/// entirely throttle wait, the on-wire RTT near zero). The default cedes per leg;
/// the `yieldn` URL param batches it for A/B measurement.
const DEFAULT_YIELD_BATCH: usize = 1;

static YIELD_BATCH: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(DEFAULT_YIELD_BATCH);

/// Resolutions counted toward the next macrotask yield, shared across every
/// concurrent prefetch leg so the yield cadence is global, not per-future.
static YIELD_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Override the prefetch yield batch size (`yieldn` URL param). The default is a
/// per-leg yield; a larger value cedes once every N resolutions for an A/B.
pub fn configure_yield_batch(n: Option<usize>) {
    if let Some(n) = n.filter(|n| *n > 0) {
        YIELD_BATCH.store(n, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Cede to the browser event loop once every `YIELD_BATCH` resolved retrievals.
///
/// Called by every prefetch leg the instant its retrieval resolves. The shared
/// counter makes the cadence global across the whole fan-out, so a batch of N
/// cedes once per N resolutions regardless of which leg resolves. The default of
/// 1 cedes per leg.
async fn maybe_yield_to_event_loop() {
    let batch = YIELD_BATCH
        .load(std::sync::atomic::Ordering::Relaxed)
        .max(1);
    let n = YIELD_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    if n.is_multiple_of(batch) {
        vertex_tasks::time::yield_to_event_loop().await;
    }
}

/// When set, `download_file` walks the chunk tree with a single global in-flight
/// pool that enqueues a node's children the instant the node decodes, instead of
/// the level-synchronous breadth-first walk that drains each level fully before
/// starting the next. The pool stays full across level boundaries (no per-level
/// barrier) but always admits the shallowest pending node first, so ancestors
/// stay warm ahead of their leaves. Toggled by the `pipeline` URL param.
static PREFETCH_PIPELINE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// When set, `download_file` follows the main prefetch with bounded wide
/// re-fetch passes over only the chunks the prefetch skipped, warming them while
/// the congesting first wave has drained, rather than leaving them for the
/// ordered joiner to grind one neighbourhood-bound subtree at a time. Toggled by
/// the `refetch` URL param.
static PREFETCH_REFETCH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Enable the post-prefetch skipped-set re-fetch passes (`refetch` URL param).
pub fn configure_prefetch_refetch(on: bool) {
    PREFETCH_REFETCH.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Enable the pipelined (no per-level barrier) prefetch (`pipeline` URL param).
pub fn configure_prefetch_pipeline(on: bool) {
    PREFETCH_PIPELINE.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Chunk reads the joiner keeps in flight while assembling from the warm cache.
///
/// With the concurrent prefetch warming the shared map ahead of it, the joiner's
/// ordered reads mostly hit the cache, so this is a lookahead window over warm
/// chunks rather than the retrieval breadth. Per-storer pressure on any
/// network miss is still bounded by the provider's per-peer cap.
const JOIN_CONCURRENCY: usize = 64;

/// Peak count of chunks held in wasm memory at once during a download.
///
/// The ordered path warms a shared map it never evicts, so this climbs to the
/// whole fetched window; the random-access path writes each leaf to disk and
/// drops it, so this stays bounded by the in-flight width. A measurement aid
/// only (`peakRetainedChunks` / `resetPeakRetainedChunks` on the client) so a
/// headless A/B can read the retained-memory high-water mark without a heap
/// snapshot.
static PEAK_RETAINED_CHUNKS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Record `current` retained chunks, bumping the peak high-water mark.
fn observe_retained(current: usize) {
    PEAK_RETAINED_CHUNKS.fetch_max(current, std::sync::atomic::Ordering::Relaxed);
}

/// The peak retained-chunk high-water mark since the last reset.
pub fn peak_retained_chunks() -> usize {
    PEAK_RETAINED_CHUNKS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Reset the retained-chunk high-water mark before a measured download.
pub fn reset_peak_retained_chunks() {
    PEAK_RETAINED_CHUNKS.store(0, std::sync::atomic::Ordering::Relaxed);
}

/// Buffered `(offset, body)` leaves the random-access sink stage may hold between
/// the fetch stream and the disk writes.
///
/// The streaming random-access path decouples fetching from the single positional
/// sink writable: the fetch stream pushes each landed leaf into a bounded channel
/// and a sink stage drains it, writing sequentially. The bound caps peak buffered
/// memory at this many leaves (plus the fetch pool's in-flight width); an
/// unbounded channel would re-buffer the whole file. Small enough to bound memory,
/// large enough that a positional write never starves the fetch pool.
const SINK_CHANNEL_CAP: usize = 128;

/// Peak depth the random-access sink channel reached, a buffered-memory gauge.
///
/// The fetch stream stalls (backpressure) once the channel is full, so this never
/// exceeds [`SINK_CHANNEL_CAP`]. A measurement aid (`peakSinkChannelDepth` /
/// `resetPeakSinkChannelDepth` on the client) proving the decoupling does not
/// reintroduce an unbounded buffer.
static PEAK_SINK_CHANNEL_DEPTH: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Record `current` queued leaves, bumping the channel-depth high-water mark.
fn observe_sink_channel_depth(current: usize) {
    PEAK_SINK_CHANNEL_DEPTH.fetch_max(current, std::sync::atomic::Ordering::Relaxed);
}

/// The peak sink-channel-depth high-water mark since the last reset.
pub fn peak_sink_channel_depth() -> usize {
    PEAK_SINK_CHANNEL_DEPTH.load(std::sync::atomic::Ordering::Relaxed)
}

/// Reset the sink-channel-depth high-water mark before a measured download.
pub fn reset_peak_sink_channel_depth() {
    PEAK_SINK_CHANNEL_DEPTH.store(0, std::sync::atomic::Ordering::Relaxed);
}

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

/// Stream the file at `root` to a random-access `sink`, resolving a single-file
/// manifest if `root` is one. Leaves are written to their exact byte offset out
/// of order, so a slow chunk never blocks the writes ready behind it.
pub async fn stream_reference_random_access(
    root: ChunkAddress,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
    sink: &RandomAccessSink,
) -> Result<(), JsValue> {
    let file_root = match probe_manifest_entries(root, provider.clone(), cache).await? {
        Some(entries) => pick_manifest_file(&entries)?,
        None => root,
    };
    stream_file_random_access(file_root, provider, sink).await
}

/// A node awaiting retrieval: its address, byte offset in the file, tree depth
/// (for ancestors-first dispatch), and how many attempts it has cost so far.
struct PendingNode {
    addr: ChunkAddress,
    offset: u64,
    depth: u32,
    attempt: u32,
}

/// Stream the file at `file_root` to `sink` with bounded-concurrency,
/// out-of-order positional writes, driven by the core joiner's offset stream.
///
/// The joiner's
/// [`into_offset_stream_chunked`](nectar_primitives::Joiner::into_offset_stream_chunked)
/// fetches the chunk tree at chunk granularity with bounded concurrency and
/// yields each leaf body tagged with its absolute byte offset the instant it
/// lands, out of order. Every intermediate and leaf fetch competes for one
/// bounded pool, so the configured width of individual chunks is in flight
/// regardless of how few top-level subtrees the tree has (the subtree-granular
/// variant would fetch only one leaf per subtree at a time).
///
/// The fetch stage and the disk sink run decoupled. A bounded channel
/// ([`SINK_CHANNEL_CAP`]) carries each landed `(offset, body)` from the fetch
/// stream to a sink stage that drains it and writes positionally, one write at a
/// time (the single OPFS/FSA writable is inherently sequential). Both stages are
/// polled concurrently by one task, so a slow positional write no longer stalls
/// the fetch pull: the stream keeps pulling and the pool stays saturated, blocking
/// only when the channel fills (backpressure). Peak wasm memory is bounded by the
/// fetch width plus the channel cap, not the file size: an unbounded channel would
/// re-buffer the whole file, which is exactly the coupling this avoids.
///
/// Per-chunk resilience (a per-attempt timeout, inline re-races, and a backoff
/// for a congested wave) lives in the [`RetryingChunkGet`] the joiner reads from:
/// a difficult chunk keeps its own concurrency slot busy retrying without gating
/// the other in-flight leaves, and the stream fails only once a chunk exhausts
/// every retry pass.
pub async fn stream_file_random_access(
    file_root: ChunkAddress,
    provider: Arc<dyn SwarmChunkProvider>,
    sink: &RandomAccessSink,
) -> Result<(), JsValue> {
    use futures::SinkExt;
    use futures::channel::mpsc;
    use nectar_primitives::bytes::Bytes;

    let getter = RetryingChunkGet::new(provider);
    let joiner = Joiner::<RetryingChunkGet, DEFAULT_BODY_SIZE>::new(getter, file_root)
        .await
        .map_err(|e| JsValue::from_str(&format!("joiner open: {e}")))?
        .with_concurrency(prefetch_concurrency().max(1));

    // The span is known once the joiner is open, written to the sink up front so
    // it can size the file and drive progress before any leaf lands.
    let total = joiner.size();
    sink.set_total_ra(total as f64);

    if total == 0 {
        return finish_ra(sink).await;
    }

    // The decoupling channel: the fetch stage sends, the sink stage receives. The
    // bound applies backpressure (a full channel stalls the fetch pull) and caps
    // buffered memory. `channel(CAP)` reserves one extra slot per sender, so the
    // effective queue depth is `CAP + 1`.
    let (mut tx, mut rx) = mpsc::channel::<(u64, Bytes)>(SINK_CHANNEL_CAP);
    // Live channel depth, shared by both stages on this single task: the fetch
    // stage increments on send, the sink stage decrements on drain. A `Cell`, not
    // an atomic, because both stages run on the one wasm thread.
    let queued = std::cell::Cell::new(0usize);

    // Fetch stage: drain the offset stream into the channel, awaiting `send` only
    // when the channel is full. Pulls keep the fetch pool saturated independently
    // of how fast the sink drains.
    let fetch_stage = async {
        let stream = joiner.into_offset_stream_chunked();
        futures::pin_mut!(stream);
        while let Some(pair) = stream.next().await {
            let (offset, body) =
                pair.map_err(|e| JsValue::from_str(&format!("joiner read: {e}")))?;
            let depth = queued.get() + 1;
            queued.set(depth);
            observe_sink_channel_depth(depth);
            // Retained leaves: the in-flight fetch width plus what the channel
            // holds, both bounded, so this never grows with the file.
            observe_retained(joiner_concurrency_hint() + depth);
            // A closed receiver means the sink stage aborted; surface that as the
            // fetch error so the join returns it.
            tx.send((offset, body))
                .await
                .map_err(|_| JsValue::from_str("sink stage closed"))?;
        }
        // Drop the sender so the sink stage's receive loop terminates.
        drop(tx);
        Ok::<(), JsValue>(())
    };

    // Sink stage: drain the channel and write each leaf to its byte offset,
    // sequentially. The single positional writable is written one leg at a time,
    // but this no longer gates fetching.
    let sink_stage = async {
        let mut written: u64 = 0;
        while let Some((offset, body)) = rx.next().await {
            queued.set(queued.get().saturating_sub(1));
            match write_leaf(sink, offset, &body).await {
                Ok(n) => written += n,
                Err(e) => {
                    // Stop receiving so the fetch stage's `send` fails fast and the
                    // join returns the write error.
                    rx.close();
                    return Err(e);
                }
            }
        }
        Ok::<u64, JsValue>(written)
    };

    // Poll both stages on one task (single-threaded wasm: no spawn, the sink stays
    // borrowed). A sink-stage write error has already aborted the sink (in
    // `write_leaf`); a fetch error aborts before propagating.
    let (fetch_result, sink_result) = futures::future::join(fetch_stage, sink_stage).await;
    let written = sink_result?;
    if let Err(e) = fetch_result {
        sink.abort_ra(&format!("{e:?}"));
        return Err(e);
    }

    debug_assert_eq!(written, total, "written bytes match the file span");
    let _ = written;
    finish_ra(sink).await
}

/// The streaming download's in-flight concurrency width, a retained-memory gauge
/// approximation: the offset stream holds at most this many leaves at once.
fn joiner_concurrency_hint() -> usize {
    prefetch_concurrency().max(1)
}

/// Stream the byte range `[offset, offset + len)` of the file at `root` to a
/// random-access `sink`, resolving a single-file manifest if `root` is one.
pub async fn stream_reference_random_access_range(
    root: ChunkAddress,
    offset: u64,
    len: u64,
    width: usize,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
    sink: &RandomAccessSink,
) -> Result<(), JsValue> {
    let file_root = match probe_manifest_entries(root, provider.clone(), cache).await? {
        Some(entries) => pick_manifest_file(&entries)?,
        None => root,
    };
    stream_range_random_access(file_root, offset, len, width, provider, sink).await
}

/// Stream the byte range `[range_start, range_end)` of the file at `file_root` to
/// `sink` with full-concurrency, out-of-order positional writes, writing each
/// overlapping leaf to its range-relative offset and dropping it once on disk.
///
/// The whole-file random-access stream buffers nothing past the in-flight pool;
/// this is its range-scoped sibling. It walks only the subtrees overlapping the
/// range (the offset/overlap arithmetic the range prefetch uses) so a worker
/// fetches just its slice, and clips a boundary leaf to the wanted bytes so the
/// staged file is exactly `range_end - range_start` long. The sink offset is
/// range-relative (`file_offset - range_start`), so a 60 MB range stages a 60 MB
/// file regardless of where in an 800 MB original it sits. The retained-chunk
/// gauge stays bounded by the width: a decoded leaf is written and dropped, never
/// accumulated like the ordered range path's shared map.
pub async fn stream_range_random_access(
    file_root: ChunkAddress,
    range_start: u64,
    len: u64,
    width: usize,
    provider: Arc<dyn SwarmChunkProvider>,
    sink: &RandomAccessSink,
) -> Result<(), JsValue> {
    use std::cmp::Reverse;
    use std::collections::{BinaryHeap, VecDeque};

    use futures::stream::FuturesUnordered;

    const BRANCHES: u64 = (DEFAULT_BODY_SIZE / REF_SIZE) as u64;
    const INLINE_RETRIES: u32 = 2;
    const MAX_RETRY_PASSES: u32 = 12;
    const RETRY_BACKOFF_STEP: std::time::Duration = std::time::Duration::from_millis(200);
    const ATTEMPT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(8000);

    let getter_provider = Arc::clone(&provider);
    let root_chunk = retrieve_with_timeout(&getter_provider, &file_root, ATTEMPT_TIMEOUT)
        .await
        .map_err(|e| JsValue::from_str(&format!("root retrieve: {e}")))?;
    let total = root_chunk.span();
    if total == 0 || range_start >= total {
        sink.set_total_ra(0.0);
        return finish_ra(sink).await;
    }
    let range_end = range_start.saturating_add(len).min(total);
    let want = range_end - range_start;
    sink.set_total_ra(want as f64);

    let limit = if width == 0 {
        prefetch_concurrency()
    } else {
        width
    }
    .max(1);
    let mut seen: HashSet<ChunkAddress> = HashSet::new();
    seen.insert(file_root);

    // Depth-ordered work queue carrying each node's absolute file offset, so a
    // boundary leaf can be clipped to the wanted bytes.
    let mut pending: BinaryHeap<Reverse<(u32, ChunkAddress, u64)>> = BinaryHeap::new();
    let mut retry: VecDeque<PendingNode> = VecDeque::new();
    let mut in_flight = FuturesUnordered::new();

    let spawn = |node: PendingNode, skip_backoff: bool| {
        let provider = Arc::clone(&provider);
        async move {
            if node.attempt > 0 && !skip_backoff {
                vertex_tasks::time::sleep(RETRY_BACKOFF_STEP * node.attempt).await;
            }
            let outcome = retrieve_with_timeout(&provider, &node.addr, ATTEMPT_TIMEOUT).await;
            maybe_yield_to_event_loop().await;
            (node, outcome)
        }
    };

    // Enqueue only the children whose byte interval overlaps the wanted range.
    let enqueue_overlapping = |pending: &mut BinaryHeap<Reverse<(u32, ChunkAddress, u64)>>,
                               seen: &mut HashSet<ChunkAddress>,
                               depth: u32,
                               node_offset: u64,
                               node_span: u64,
                               body: &[u8]|
     -> Result<(), JsValue> {
        let child_span = child_subtree_span(node_span, BRANCHES);
        for (i, child) in parse_child_refs(body)?.into_iter().enumerate() {
            let child_offset = node_offset + (i as u64) * child_span;
            let child_end = child_offset + child_span;
            if child_end <= range_start || child_offset >= range_end {
                continue;
            }
            if seen.insert(child) {
                pending.push(Reverse((depth, child, child_offset)));
            }
        }
        Ok(())
    };

    let mut written: u64 = 0;
    if root_chunk.span() > DEFAULT_BODY_SIZE as u64 {
        enqueue_overlapping(
            &mut pending,
            &mut seen,
            1,
            0,
            root_chunk.span(),
            root_chunk.data(),
        )?;
    } else {
        written += write_clipped_leaf(sink, 0, root_chunk.data(), range_start, range_end).await?;
    }

    macro_rules! refill {
        () => {{
            while in_flight.len() < limit {
                if let Some(Reverse((depth, addr, offset))) = pending.pop() {
                    in_flight.push(spawn(
                        PendingNode {
                            addr,
                            offset,
                            depth,
                            attempt: 0,
                        },
                        false,
                    ));
                } else if let Some(node) = retry.pop_front() {
                    in_flight.push(spawn(node, false));
                } else {
                    break;
                }
            }
        }};
    }

    refill!();

    // Total leaves parked on the retry queue, and writes completed: a measurement
    // aid (`ra-range` log lines) proving the parked nodes never gate the ready
    // writes, which keep landing while retries are pending.
    let mut parked: u64 = 0;
    let mut writes: u64 = 0;

    while let Some((node, outcome)) = in_flight.next().await {
        // The retained-memory high-water mark is the pool size: a decoded leaf is
        // written to disk and dropped, never accumulated, so this is bounded by
        // the width rather than growing with the range.
        observe_retained(in_flight.len() + 1);
        let chunk = match outcome {
            Ok(chunk) => chunk,
            Err(ref e) => {
                let next_attempt = node.attempt + 1;
                if next_attempt < INLINE_RETRIES {
                    let skip_backoff = !e.is_not_found();
                    in_flight.push(spawn(
                        PendingNode {
                            attempt: next_attempt,
                            ..node
                        },
                        skip_backoff,
                    ));
                    continue;
                }
                if next_attempt < MAX_RETRY_PASSES {
                    parked += 1;
                    // Park and free the slot for a ready chunk: the write loop
                    // keeps draining while this difficult node waits its backoff.
                    tracing::info!(
                        "ra-range-park addr={} attempt={next_attempt} retry_depth={} writes={writes}",
                        node.addr,
                        retry.len() + 1
                    );
                    retry.push_back(PendingNode {
                        attempt: next_attempt,
                        ..node
                    });
                    refill!();
                    continue;
                }
                sink.abort_ra(&format!("retrieve {}: {e}", node.addr));
                return Err(JsValue::from_str(&format!(
                    "retrieve {} exhausted retries: {e}",
                    node.addr
                )));
            }
        };

        if chunk.span() > DEFAULT_BODY_SIZE as u64 {
            enqueue_overlapping(
                &mut pending,
                &mut seen,
                node.depth + 1,
                node.offset,
                chunk.span(),
                chunk.data(),
            )?;
        } else {
            let n =
                write_clipped_leaf(sink, node.offset, chunk.data(), range_start, range_end).await?;
            if n > 0 {
                written += n;
                writes += 1;
                // Surface a write that landed while a node was parked: the
                // retry_depth being non-zero here is the non-blocking proof.
                if writes.is_multiple_of(500) {
                    tracing::info!(
                        "ra-range-progress writes={writes} written={written} retry_depth={} pending={} inflight={}",
                        retry.len(),
                        pending.len(),
                        in_flight.len()
                    );
                }
            }
        }
        refill!();
    }

    tracing::info!(
        "ra-range-done want={want} written={written} writes={writes} parked_total={parked}"
    );
    debug_assert_eq!(written, want, "written bytes match the wanted range");
    finish_ra(sink).await
}

/// Write the portion of a leaf at file `leaf_offset` that overlaps
/// `[range_start, range_end)`, at the range-relative sink offset. Returns the
/// bytes written (0 if the leaf lies wholly outside the range).
async fn write_clipped_leaf(
    sink: &RandomAccessSink,
    leaf_offset: u64,
    body: &[u8],
    range_start: u64,
    range_end: u64,
) -> Result<u64, JsValue> {
    let leaf_end = leaf_offset + body.len() as u64;
    let lo = leaf_offset.max(range_start);
    let hi = leaf_end.min(range_end);
    if lo >= hi {
        return Ok(0);
    }
    let body_lo = (lo - leaf_offset) as usize;
    let body_hi = (hi - leaf_offset) as usize;
    write_leaf(sink, lo - range_start, &body[body_lo..body_hi]).await
}

/// Write a leaf body at `offset`, returning the bytes written. Maps a sink write
/// failure to a `JsValue` error after aborting the sink.
async fn write_leaf(sink: &RandomAccessSink, offset: u64, body: &[u8]) -> Result<u64, JsValue> {
    let view = Uint8Array::from(body);
    sink.write_at(offset as f64, view).await.map_err(|e| {
        sink.abort_ra("write failed");
        JsValue::from_str(&format!("sink write at {offset}: {e:?}"))
    })?;
    Ok(body.len() as u64)
}

/// Close a random-access `sink`, mapping a close failure to a `JsValue` error.
async fn finish_ra(sink: &RandomAccessSink) -> Result<(), JsValue> {
    sink.close_ra()
        .await
        .map(|_| ())
        .map_err(|e| JsValue::from_str(&format!("sink close: {e:?}")))
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
    if PREFETCH_PIPELINE.load(std::sync::atomic::Ordering::Relaxed) {
        prefetch_tree_pipelined(file_root, provider.clone(), cache).await?;
    } else {
        prefetch_tree(file_root, provider.clone(), cache).await?;
    }

    if PREFETCH_REFETCH.load(std::sync::atomic::Ordering::Relaxed) {
        warm_skipped(file_root, provider.clone(), cache).await?;
    }

    let getter = NetworkChunkGet::new(provider, cache.snapshot_map());
    join_to_bytes(file_root, getter).await
}

/// Wide re-fetch passes over the chunks the main prefetch left uncached.
///
/// The first prefetch wave congests the close peers and skips the hardest
/// (deepest-forwarding) chunks. Left to the ordered joiner those skips serialise
/// the tail, because they cluster on the same few close peers. This warms them in
/// wide unordered passes after the wave has drained: each pass fetches only the
/// still-missing addresses, and passes repeat until the set is empty or a pass
/// makes no progress (a genuinely unreachable chunk is then left for the joiner's
/// own retrieval). Enumerating the tree reads warm intermediates from the cache,
/// so the walk costs nothing for the already-prefetched majority.
async fn warm_skipped(
    file_root: ChunkAddress,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
) -> Result<(), JsValue> {
    // Bound the passes so a permanently unreachable chunk cannot loop forever;
    // each pass that fetches at least one chunk resets the no-progress count.
    const MAX_PASSES: usize = 8;

    let all = list_tree_addresses(file_root, provider.clone(), cache, 0).await?;
    for pass in 0..MAX_PASSES {
        let missing: Vec<ChunkAddress> = all
            .iter()
            .copied()
            .filter(|a| cache.fetch(a).is_none())
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        tracing::info!("warm-skipped pass={pass} missing={}", missing.len());
        let fetched: usize = futures::stream::iter(missing.into_iter())
            .map(|addr| {
                let provider = Arc::clone(&provider);
                async move {
                    match provider.retrieve_chunk(&addr).await {
                        Ok(r) => {
                            maybe_yield_to_event_loop().await;
                            Some(r.chunk)
                        }
                        Err(_) => None,
                    }
                }
            })
            .buffer_unordered(prefetch_concurrency())
            .filter_map(|c| async move { c })
            .fold(0usize, |n, chunk| {
                cache.insert(chunk);
                async move { n + 1 }
            })
            .await;
        // No chunk landed this pass: the remainder is unreachable from here, so
        // stop and let the joiner make its own attempt rather than spinning.
        if fetched == 0 {
            tracing::info!("warm-skipped no progress at pass={pass}; leaving remainder for joiner");
            return Ok(());
        }
    }
    Ok(())
}

/// Resolve the manifest at `root` to its file root, then breadth-first walk the
/// chunk tree returning chunk addresses. Only intermediates are fetched (a leaf
/// has no children to enumerate), so the walk costs one round trip per
/// intermediate, not per leaf. `max_addresses` caps the collected set (0 = whole
/// tree); the worker-sharded download partitions this set across worker nodes.
pub async fn list_tree_addresses(
    root: ChunkAddress,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
    max_addresses: usize,
) -> Result<Vec<ChunkAddress>, JsValue> {
    let file_root = match probe_manifest_entries(root, provider.clone(), cache).await? {
        Some(entries) => pick_manifest_file(&entries)?,
        None => root,
    };

    // Only intermediate nodes need fetching to enumerate children; a transient
    // forwarding miss on one must not abort the whole walk, so each is retried a
    // few times with a short backoff (the same congestion that fails the wide
    // download fan-out also fails a cold intermediate fetch).
    const LIST_RETRIES: u32 = 3;
    const LIST_BACKOFF: std::time::Duration = std::time::Duration::from_millis(200);

    let mut out: Vec<ChunkAddress> = Vec::new();
    let mut seen: HashSet<ChunkAddress> = HashSet::new();
    seen.insert(file_root);

    // `level` holds only nodes that must be fetched to enumerate their children
    // (intermediates). Leaf addresses are recorded from their parent without a
    // fetch, so the walk costs one round trip per intermediate, never per leaf.
    let mut level: Vec<ChunkAddress> = vec![file_root];
    out.push(file_root);

    // A very large file has thousands of intermediates; enumerating the whole
    // tree on a cold node is slow. A bounded sample of leaf addresses is enough
    // to keep the shard workers saturated for a throughput measurement, so stop
    // descending once `max_addresses` are collected (0 disables the cap).
    while !level.is_empty() {
        if max_addresses != 0 && out.len() >= max_addresses {
            break;
        }
        let fetched: Vec<Result<AnyChunk, JsValue>> = futures::stream::iter(level.iter().copied())
            .map(|addr| {
                let provider = Arc::clone(&provider);
                let cached = cache.fetch(&addr);
                async move {
                    if let Some(chunk) = cached {
                        return Ok(chunk);
                    }
                    let mut last = String::new();
                    for attempt in 0..LIST_RETRIES {
                        if attempt > 0 {
                            vertex_tasks::time::sleep(LIST_BACKOFF * attempt).await;
                        }
                        match provider.retrieve_chunk(&addr).await {
                            Ok(r) => {
                                // Cede between resolved retrievals so a batch does
                                // not drain in one synchronous pass on the single
                                // browser thread and starve the swarm run loop.
                                maybe_yield_to_event_loop().await;
                                return Ok(r.chunk);
                            }
                            Err(e) => last = e.to_string(),
                        }
                    }
                    Err(JsValue::from_str(&format!("retrieve {addr}: {last}")))
                }
            })
            .buffer_unordered(prefetch_concurrency())
            .collect()
            .await;

        let mut next: Vec<ChunkAddress> = Vec::new();
        for res in fetched {
            // A cold worker may exhaust the retries on a deep intermediate; skip
            // its subtree rather than abort the whole enumeration, so the shard
            // set is a large best-effort sample (a throughput measure does not
            // need every last leaf).
            let chunk = match res {
                Ok(chunk) => chunk,
                Err(_) => continue,
            };
            // A leaf node (`span <= body size`) has no children; only an
            // intermediate is decoded for child refs.
            if chunk.span() > DEFAULT_BODY_SIZE as u64 {
                let children = parse_child_refs(chunk.data())?;
                let num = children.len().max(1) as u64;
                // The Swarm chunk tree is balanced, so a node whose span spread
                // across its children is at most one body each has leaf children:
                // record them without a fetch. Otherwise the children are
                // themselves intermediates that must be fetched to descend.
                let children_are_leaves = chunk.span().div_ceil(num) <= DEFAULT_BODY_SIZE as u64;
                for child in children {
                    if seen.insert(child) {
                        out.push(child);
                        if !children_are_leaves {
                            next.push(child);
                        }
                    }
                }
            }
            cache.insert(chunk);
        }
        level = next;
    }
    Ok(out)
}

/// Prefetch the chunk tree into `cache` with no per-level barrier, admitting the
/// shallowest pending node first.
///
/// A single in-flight pool holds up to `prefetch_concurrency()` retrievals.
/// Whenever an intermediate node decodes, its not-yet-seen children are queued
/// immediately rather than after the rest of the node's level finishes, so the
/// pool stays full across level boundaries and one slow leg never stalls a whole
/// level. The pending queue is a min-heap on tree depth, so a freed slot always
/// takes the shallowest waiting node: ancestors are dispatched ahead of their
/// leaves, preserving the level walk's "warm ancestors first" ordering without
/// its hard per-level drain. Equivalent to `prefetch_tree` in what it fetches; it
/// differs only in scheduling.
async fn prefetch_tree_pipelined(
    root: ChunkAddress,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
) -> Result<(), JsValue> {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    use futures::stream::FuturesUnordered;

    // A pipelined leaf reaches the network earlier than under the level walk, so
    // the wide fan-out can momentarily congest the close peers and a chunk's whole
    // candidate race fails on transient transport errors rather than chunk absence
    // (the level walk masks this because it dispatches a level only once its
    // ancestors are warm, never saturating the network as hard). Each address is
    // retried up to this many times, with a backoff between retries so the
    // congestion clears before the chunk re-races, before the download fails.
    const MAX_CHUNK_RETRIES: u32 = 6;
    // Backoff before re-racing a failed chunk, grown per attempt so a congested
    // wave drains before the retry re-hammers the same close peers.
    const RETRY_BACKOFF_STEP: std::time::Duration = std::time::Duration::from_millis(150);

    let mut seen: HashSet<ChunkAddress> = HashSet::new();
    seen.insert(root);

    let limit = prefetch_concurrency().max(1);
    // Min-heap on depth: `Reverse((depth, addr))` pops the shallowest node, so
    // freed slots always go to ancestors before their deeper descendants.
    let mut pending: BinaryHeap<Reverse<(u32, ChunkAddress)>> = BinaryHeap::new();
    pending.push(Reverse((0, root)));
    let mut in_flight = FuturesUnordered::new();

    let spawn = |addr: ChunkAddress, depth: u32, attempt: u32, skip_backoff: bool| {
        let provider = Arc::clone(&provider);
        let cached = cache.fetch(&addr);
        async move {
            // Back off before a retry so a congested wave drains; the first
            // attempt (0) runs immediately. A retry after a connectivity failure
            // skips the backoff: the prior peers are gone (the scheduler now routes
            // around just-reset peers), so re-racing a fresh live peer at once,
            // rather than parking the chunk behind the dead connection, recovers
            // the in-flight work the reset lost. Backoff is kept for a not-found
            // retry, where the chunk's ancestors may simply need to warm first.
            if attempt > 0 && !skip_backoff {
                vertex_tasks::time::sleep(RETRY_BACKOFF_STEP * attempt).await;
            }
            let outcome = match cached {
                Some(chunk) => Ok(chunk),
                None => {
                    let r = provider.retrieve_chunk(&addr).await.map(|r| r.chunk);
                    // Cede between resolved retrievals so the pool drain does not
                    // monopolise the single browser thread and starve the swarm
                    // run loop that feeds the next responses.
                    maybe_yield_to_event_loop().await;
                    r
                }
            };
            (addr, depth, attempt, outcome)
        }
    };

    // Prime the pool, then refill from `pending` as slots free.
    while in_flight.len() < limit {
        match pending.pop() {
            Some(Reverse((depth, addr))) => in_flight.push(spawn(addr, depth, 0, false)),
            None => break,
        }
    }

    let mut skipped = 0usize;
    while let Some((addr, depth, attempt, outcome)) = in_flight.next().await {
        let chunk = match outcome {
            Ok(chunk) => chunk,
            Err(ref e) if attempt + 1 < MAX_CHUNK_RETRIES => {
                // Re-dispatch a connectivity failure at once (the dead peers are on
                // cooldown, so the re-race lands on a live peer); keep the backoff
                // for a not-found, which is coverage, not a lost connection.
                let skip_backoff = !e.is_not_found();
                in_flight.push(spawn(addr, depth, attempt + 1, skip_backoff));
                continue;
            }
            // Prefetch is a cache-warming optimisation, not the correctness path:
            // a chunk that exhausts its retries (transient congestion against a
            // deep-forwarding chunk) is left for the joiner's own retrieval, which
            // reaches it later once the burst has drained and its ancestors are
            // warm. Skipping an intermediate node forgoes warming its subtree; the
            // joiner re-fetches that node and walks down. Never abort the whole
            // download on one prefetch miss.
            Err(_) => {
                skipped += 1;
                // Refill the freed slot before continuing so the pool stays full.
                while in_flight.len() < limit {
                    match pending.pop() {
                        Some(Reverse((depth, addr))) => {
                            in_flight.push(spawn(addr, depth, 0, false))
                        }
                        None => break,
                    }
                }
                continue;
            }
        };
        if chunk.span() > DEFAULT_BODY_SIZE as u64 {
            for child in parse_child_refs(chunk.data())? {
                if seen.insert(child) {
                    pending.push(Reverse((depth + 1, child)));
                }
            }
        }
        cache.insert(chunk);
        // Refill freed slots, shallowest-first, from the work queue.
        while in_flight.len() < limit {
            match pending.pop() {
                Some(Reverse((depth, addr))) => in_flight.push(spawn(addr, depth, 0, false)),
                None => break,
            }
        }
    }

    if skipped > 0 {
        tracing::info!("prefetch-skipped skipped={skipped} (pipelined left for joiner)");
    }
    Ok(())
}

/// Resolve `root` to its file root: if `root` is a single-file manifest, return
/// the contained file's root; otherwise `root` is already a file root.
///
/// The worker-sharded download resolves this once on one worker and hands the
/// file root to every worker, so the K range downloads skip the manifest probe.
pub async fn resolve_file_root(
    root: ChunkAddress,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
) -> Result<ChunkAddress, JsValue> {
    match probe_manifest_entries(root, provider.clone(), cache).await? {
        Some(entries) => pick_manifest_file(&entries),
        None => Ok(root),
    }
}

/// Total byte size of the file at `file_root` (opens the joiner, reads its span).
pub async fn file_size(
    file_root: ChunkAddress,
    provider: Arc<dyn SwarmChunkProvider>,
) -> Result<u64, JsValue> {
    let getter = NetworkChunkGet::new(provider, HashMap::new());
    let joiner = Joiner::<NetworkChunkGet, DEFAULT_BODY_SIZE>::new(getter, file_root)
        .await
        .map_err(|e| JsValue::from_str(&format!("joiner open: {e}")))?;
    Ok(joiner.size())
}

/// Download the byte range `[offset, offset + len)` of the file at `file_root`.
///
/// Runs the same wide concurrent prefetch the monolithic streamed path uses, but
/// scoped to the subtrees that overlap the range, so a worker fetches only the
/// chunks for its slice. The joiner's `read_range` then assembles those bytes
/// from the warm map. Returns the slice bytes; the coordinator writes them at
/// `offset` to reassemble the file.
pub async fn download_range(
    file_root: ChunkAddress,
    offset: u64,
    len: u64,
    width: usize,
    provider: Arc<dyn SwarmChunkProvider>,
) -> Result<Vec<u8>, JsValue> {
    let getter = NetworkChunkGet::new(provider, HashMap::new());
    let shared = getter.shared();
    let provider = getter.provider();
    let joiner = Joiner::<NetworkChunkGet, DEFAULT_BODY_SIZE>::new(getter, file_root)
        .await
        .map_err(|e| JsValue::from_str(&format!("joiner open: {e}")))?
        .with_concurrency(JOIN_CONCURRENCY);

    let size = joiner.size();
    if size == 0 || offset >= size {
        return Ok(Vec::new());
    }
    let end = offset.saturating_add(len).min(size);
    let want = (end - offset) as usize;

    // Warm only the chunks overlapping [offset, end), then read the range from the
    // warm map. The prefetch width is per-worker: a worker node holds a small peer
    // set, so a width tuned to its connected fan keeps the fan-out from collapsing
    // its own neighbourhood (the dial-storm that caps a single wide-prefetch node).
    let width = if width == 0 {
        prefetch_concurrency()
    } else {
        width
    };
    prefetch_range_into_shared(file_root, offset, end, width, provider, shared).await?;
    joiner
        .read_range(offset, want)
        .await
        .map_err(|e| JsValue::from_str(&format!("joiner read_range: {e}")))
}

/// Prefetch into `shared` only the subtrees overlapping the byte range
/// `[range_start, range_end)`, breadth-first at the configured width.
///
/// Each node carries its own byte offset and span; a child whose byte interval
/// does not overlap the range is never queued, so a worker fetches just its
/// slice's chunks (plus the ancestor intermediates on the path to them). Misses
/// are non-fatal: a chunk left unwarmed is fetched by the joiner's own read.
async fn prefetch_range_into_shared(
    root: ChunkAddress,
    range_start: u64,
    range_end: u64,
    width: usize,
    provider: Arc<dyn SwarmChunkProvider>,
    shared: Arc<Mutex<HashMap<ChunkAddress, AnyChunk>>>,
) -> Result<(), JsValue> {
    // Branching factor of a plain intermediate node: child refs packed per body.
    const BRANCHES: u64 = (DEFAULT_BODY_SIZE / REF_SIZE) as u64;
    // A congested wave fails a chunk's whole candidate race on a transient
    // transport error rather than absence; re-race it after a short backoff
    // before giving up, so the slice does not stall on a recoverable miss. A
    // single retrieval can also hang indefinitely when a worker's neighbourhood
    // momentarily drains (every close storer rejects the dial), so each attempt
    // is bounded by a timeout that bounces the request and re-races it once the
    // peer set recovers, instead of leaving an in-flight future pending forever.
    const MAX_CHUNK_RETRIES: u32 = 10;
    const RETRY_BACKOFF_STEP: std::time::Duration = std::time::Duration::from_millis(250);
    const ATTEMPT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(8000);

    let mut seen: HashSet<ChunkAddress> = HashSet::new();
    seen.insert(root);
    // (address, byte_offset) for nodes whose subtree overlaps the range.
    let mut level: Vec<(ChunkAddress, u64)> = vec![(root, 0)];

    while !level.is_empty() {
        let fetched: Vec<Result<(AnyChunk, u64), JsValue>> =
            futures::stream::iter(level.into_iter())
                .map(|(addr, node_offset)| {
                    let provider = Arc::clone(&provider);
                    let cached = shared.lock().expect("cache mutex").get(&addr).cloned();
                    async move {
                        if let Some(chunk) = cached {
                            return Ok((chunk, node_offset));
                        }
                        let mut last = String::new();
                        let mut skip_backoff = false;
                        for attempt in 0..MAX_CHUNK_RETRIES {
                            if attempt > 0 && !skip_backoff {
                                vertex_tasks::time::sleep(RETRY_BACKOFF_STEP * attempt).await;
                            }
                            match retrieve_with_timeout(&provider, &addr, ATTEMPT_TIMEOUT).await {
                                Ok(chunk) => {
                                    // Cede to the event loop before this future
                                    // resolves and `buffer_unordered` re-polls the
                                    // next ready one. On the single browser thread
                                    // a wide fan-out otherwise drains a whole batch
                                    // of resolved retrievals in one synchronous
                                    // microtask pass, starving the swarm run loop
                                    // and the socket reads that feed it; the
                                    // macrotask yield lets the node deliver the
                                    // next responses between chunks.
                                    maybe_yield_to_event_loop().await;
                                    return Ok((chunk, node_offset));
                                }
                                Err(e) => {
                                    skip_backoff = !e.is_not_found();
                                    last = e.to_string();
                                }
                            }
                        }
                        Err(JsValue::from_str(&format!("retrieve {addr}: {last}")))
                    }
                })
                .buffer_unordered(width.max(1))
                .collect()
                .await;

        let mut next: Vec<(ChunkAddress, u64)> = Vec::new();
        for result in fetched {
            // A leaf that exhausts its retries is left for the joiner's own read
            // (the joiner re-fetches a cold leaf); never abort the whole slice on
            // one prefetch miss.
            let (chunk, node_offset) = match result {
                Ok(v) => v,
                Err(_) => continue,
            };
            if chunk.span() > DEFAULT_BODY_SIZE as u64 {
                // Per-child subtree span: the largest power-of-branches multiple
                // of the body size strictly below this node's span. Child i then
                // covers [node_offset + i*child_span, +child_span).
                let child_span = child_subtree_span(chunk.span(), BRANCHES);
                for (i, child) in parse_child_refs(chunk.data())?.into_iter().enumerate() {
                    let child_offset = node_offset + (i as u64) * child_span;
                    let child_end = child_offset + child_span;
                    // Skip a child subtree that lies wholly outside the range.
                    if child_end <= range_start || child_offset >= range_end {
                        continue;
                    }
                    if seen.insert(child) {
                        next.push((child, child_offset));
                    }
                }
            }
            let retained = {
                let mut guard = shared.lock().expect("cache mutex");
                guard.insert(*chunk.address(), chunk);
                guard.len()
            };
            observe_retained(retained);
        }
        level = next;
    }

    Ok(())
}

/// Retrieve one chunk, bouncing the request if it does not resolve within
/// `timeout`. A single retrieval can hang indefinitely when the worker's
/// neighbourhood drains (every close storer rejects the dial); the timeout lets
/// the caller re-race it once the peer set recovers rather than block forever.
async fn retrieve_with_timeout(
    provider: &Arc<dyn SwarmChunkProvider>,
    addr: &ChunkAddress,
    timeout: std::time::Duration,
) -> Result<AnyChunk, vertex_swarm_api::SwarmError> {
    // A `gloo` (`setTimeout`) timer, not `futures-timer`: the latter does not
    // fire while the single browser thread is saturated, so the timeout would
    // never bounce a hung retrieval. With the per-leg event-loop yield in the
    // provider this timer now elapses and the caller re-races a stuck request.
    let fetch = std::pin::pin!(provider.retrieve_chunk(addr));
    let delay = std::pin::pin!(vertex_tasks::time::sleep(timeout));
    match futures::future::select(fetch, delay).await {
        futures::future::Either::Left((Ok(r), _)) => Ok(r.chunk),
        futures::future::Either::Left((Err(e), _)) => Err(e),
        futures::future::Either::Right(_) => Err(vertex_swarm_api::SwarmError::network_msg(
            "retrieval timed out",
        )),
    }
}

/// A leaf chunk with the byte offset and length it occupies in the joined file.
///
/// The shard coordinator partitions these by address across workers; each leaf
/// carries its own offset so a worker that fetched it can return bytes the
/// coordinator places without any tree context.
pub struct LeafOffset {
    pub address: ChunkAddress,
    pub offset: u64,
}

/// Enumerate the leaves of the file at `file_root` in tree order, each tagged
/// with the byte offset and length it occupies in the joined file.
///
/// Walks the intermediate nodes breadth-first (one fetch per intermediate, never
/// per leaf), computing each child's byte offset from the parent offset and the
/// per-child subtree span, the same arithmetic the range prefetch uses. The
/// result lets the coordinator shard leaves by address and reassemble by offset:
/// each worker fetches the leaves nearest its own overlay, so the closest peer to
/// each fetched chunk is in its connected set and the not-connected tax falls.
pub async fn list_leaf_offsets(
    file_root: ChunkAddress,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
) -> Result<Vec<LeafOffset>, JsValue> {
    const BRANCHES: u64 = (DEFAULT_BODY_SIZE / REF_SIZE) as u64;
    const LIST_RETRIES: u32 = 4;
    const LIST_BACKOFF: std::time::Duration = std::time::Duration::from_millis(200);

    let mut leaves: Vec<LeafOffset> = Vec::new();
    // (intermediate address, byte offset of its subtree).
    let mut level: Vec<(ChunkAddress, u64)> = vec![(file_root, 0)];
    let mut seen: HashSet<ChunkAddress> = HashSet::new();
    seen.insert(file_root);

    while !level.is_empty() {
        let fetched: Vec<Result<(AnyChunk, u64), JsValue>> =
            futures::stream::iter(level.into_iter())
                .map(|(addr, node_offset)| {
                    let provider = Arc::clone(&provider);
                    let cached = cache.fetch(&addr);
                    async move {
                        if let Some(chunk) = cached {
                            return Ok((chunk, node_offset));
                        }
                        let mut last = String::new();
                        for attempt in 0..LIST_RETRIES {
                            if attempt > 0 {
                                vertex_tasks::time::sleep(LIST_BACKOFF * attempt).await;
                            }
                            match provider.retrieve_chunk(&addr).await {
                                Ok(r) => {
                                    maybe_yield_to_event_loop().await;
                                    return Ok((r.chunk, node_offset));
                                }
                                Err(e) => last = e.to_string(),
                            }
                        }
                        Err(JsValue::from_str(&format!(
                            "list-leaf retrieve {addr}: {last}"
                        )))
                    }
                })
                .buffer_unordered(prefetch_concurrency())
                .collect()
                .await;

        let mut next: Vec<(ChunkAddress, u64)> = Vec::new();
        for result in fetched {
            let (chunk, node_offset) = match result {
                Ok(v) => v,
                Err(_) => continue,
            };
            if chunk.span() > DEFAULT_BODY_SIZE as u64 {
                // Each child subtree spans `child_span` bytes; when that equals one
                // body the children are leaves (record their offsets without a
                // fetch), otherwise they are intermediates to descend into. This
                // is the exact structural test the range prefetch uses, never a
                // span/branch-count guess (which mis-sizes the final partial
                // child and drops leaves).
                let child_span = child_subtree_span(chunk.span(), BRANCHES);
                let children_are_leaves = child_span == DEFAULT_BODY_SIZE as u64;
                for (i, child) in parse_child_refs(chunk.data())?.into_iter().enumerate() {
                    let child_offset = node_offset + (i as u64) * child_span;
                    if children_are_leaves {
                        // A leaf address can legitimately recur at several offsets
                        // (identical 4 KiB blocks dedup to one chunk address), so
                        // every leaf occurrence is recorded by offset; only the BFS
                        // over intermediates dedups, to avoid re-walking a shared
                        // subtree.
                        leaves.push(LeafOffset {
                            address: child,
                            offset: child_offset,
                        });
                    } else if seen.insert(child) {
                        next.push((child, child_offset));
                    }
                }
            } else {
                // The root itself is a single-leaf file.
                leaves.push(LeafOffset {
                    address: *chunk.address(),
                    offset: node_offset,
                });
            }
            cache.insert(chunk);
        }
        level = next;
    }

    leaves.sort_by_key(|l| l.offset);
    Ok(leaves)
}

/// Fetch each address in `addrs` and return its body bytes paired with the byte
/// `offset` it occupies in the file, so the coordinator can reassemble without
/// tree context. Misses are retried; a leaf that exhausts its retries is skipped
/// and reported by absence (the coordinator counts assembled bytes).
pub async fn fetch_leaves_at(
    addrs: Vec<(ChunkAddress, u64)>,
    width: usize,
    provider: Arc<dyn SwarmChunkProvider>,
) -> Vec<(u64, Vec<u8>)> {
    const MAX_RETRIES: u32 = 10;
    const BACKOFF_STEP: std::time::Duration = std::time::Duration::from_millis(250);
    const ATTEMPT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(8000);

    futures::stream::iter(addrs.into_iter())
        .map(|(addr, offset)| {
            let provider = Arc::clone(&provider);
            async move {
                let mut skip_backoff = false;
                for attempt in 0..MAX_RETRIES {
                    if attempt > 0 && !skip_backoff {
                        vertex_tasks::time::sleep(BACKOFF_STEP * attempt).await;
                    }
                    match retrieve_with_timeout(&provider, &addr, ATTEMPT_TIMEOUT).await {
                        Ok(chunk) => {
                            maybe_yield_to_event_loop().await;
                            // `data()` is the leaf body (the span lives separately),
                            // so the file bytes at this offset are the body verbatim.
                            return Some((offset, chunk.data().to_vec()));
                        }
                        Err(e) => {
                            skip_backoff = !e.is_not_found();
                            continue;
                        }
                    }
                }
                None
            }
        })
        .buffer_unordered(width.max(1))
        .filter_map(|r| async move { r })
        .collect()
        .await
}

/// The byte span each child subtree of a node covering `span` bytes holds: the
/// largest `DEFAULT_BODY_SIZE * BRANCHES^k` that is strictly less than `span`.
fn child_subtree_span(span: u64, branches: u64) -> u64 {
    let mut child = DEFAULT_BODY_SIZE as u64;
    while child * branches < span {
        child *= branches;
    }
    child
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

/// Resolve `path` in the manifest at `root` to the referenced file's root
/// address, without downloading the file. The shard coordinator hands this root
/// to every worker so each range download targets the path's file directly.
pub async fn resolve_file_path(
    root: ChunkAddress,
    path: &str,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
) -> Result<ChunkAddress, JsValue> {
    let path_owned = path.to_string();
    let entry: Entry = prefetch_then(provider, cache, |c| {
        let mut manifest: PlainManifest<MemoryCache> = PlainManifest::open(root, c.clone());
        manifest.lookup(&path_owned)
    })
    .await?;

    entry
        .address()
        .copied()
        .ok_or_else(|| JsValue::from_str(&format!("manifest entry '{path}' has no reference")))
}

/// Walk `path` in the manifest at `root`, returning the referenced file's bytes.
pub async fn walk(
    root: ChunkAddress,
    path: &str,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
) -> Result<Vec<u8>, JsValue> {
    let file_root = resolve_file_path(root, path, provider.clone(), cache).await?;
    download_file(file_root, provider, cache).await
}

/// Prefetch the chunk tree at `root` into `cache`, breadth-first and concurrent.
async fn prefetch_tree(
    root: ChunkAddress,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
) -> Result<(), JsValue> {
    // A congested wave can fail a chunk's whole candidate race on a transient
    // transport error (or hang a single retrieval when a neighbourhood drains), so
    // each cold fetch retries with backoff under a per-attempt timeout, matching
    // the range path's bounds.
    const MAX_CHUNK_RETRIES: u32 = 10;
    const RETRY_BACKOFF_STEP: std::time::Duration = std::time::Duration::from_millis(250);
    const ATTEMPT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(8000);

    // Addresses whose chunk we have already fetched (or queued to fetch) this
    // pass: dedups shared subtrees and guards against a malformed cycle.
    let mut seen: HashSet<ChunkAddress> = HashSet::new();
    // The current level to fetch concurrently; starts with just the root.
    let mut level: Vec<ChunkAddress> = vec![root];
    seen.insert(root);

    let mut skipped = 0usize;
    while !level.is_empty() {
        // Fetch this whole level concurrently, skipping chunks already cached.
        let fetched: Vec<Result<AnyChunk, JsValue>> = futures::stream::iter(level.into_iter())
            .map(|addr| {
                let provider = Arc::clone(&provider);
                let cached = cache.fetch(&addr);
                async move {
                    if let Some(chunk) = cached {
                        return Ok(chunk);
                    }
                    let mut last = String::new();
                    // A connectivity failure means the served peers are gone (the
                    // scheduler now cools just-reset peers), so the next attempt
                    // re-races a live peer at once: skip the backoff and only pay it
                    // for a not-found, where the chunk's ancestors may need warming.
                    let mut skip_backoff = false;
                    for attempt in 0..MAX_CHUNK_RETRIES {
                        if attempt > 0 && !skip_backoff {
                            vertex_tasks::time::sleep(RETRY_BACKOFF_STEP * attempt).await;
                        }
                        match retrieve_with_timeout(&provider, &addr, ATTEMPT_TIMEOUT).await {
                            Ok(chunk) => {
                                // Cede to the event loop so a batch of resolved
                                // retrievals does not drain in one synchronous pass
                                // on the single browser thread, starving the swarm
                                // run loop that feeds the next responses.
                                maybe_yield_to_event_loop().await;
                                return Ok(chunk);
                            }
                            Err(e) => {
                                skip_backoff = !e.is_not_found();
                                last = e.to_string();
                            }
                        }
                    }
                    Err(JsValue::from_str(&format!("retrieve {addr}: {last}")))
                }
            })
            .buffer_unordered(prefetch_concurrency())
            .collect()
            .await;

        // Insert the fetched chunks and gather the next level (children of the
        // intermediate nodes). A chunk that exhausts its retries is a cache-warming
        // miss, not a correctness failure: leave it for the joiner's own retrieval,
        // which reaches it later once the burst has drained. Skipping an
        // intermediate forgoes warming its subtree; the joiner re-fetches it and
        // walks down. Never abort the whole download on one prefetch miss.
        let mut next: Vec<ChunkAddress> = Vec::new();
        for result in fetched {
            let chunk = match result {
                Ok(chunk) => chunk,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
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

    if skipped > 0 {
        tracing::info!("prefetch-skipped skipped={skipped} (level-walk left for joiner)");
    }
    Ok(())
}

/// Prefetch the chunk tree at `root` into the joiner's live `shared` map with a
/// single pipelined in-flight pool (no per-level barrier).
///
/// Writes into the getter's `Arc<Mutex<_>>` rather than the `MemoryCache`, so
/// the joiner streaming the same getter sees each chunk the moment it lands and
/// never re-fetches it. A depth-ordered work queue dispatches ancestors before
/// descendants so the intermediate spine is discovered fast, but leaves start as
/// soon as their parent resolves rather than after the whole intermediate level
/// drains, so the fan-out ramps to full width without stalling on the slowest
/// intermediate.
async fn prefetch_into_shared(
    root: ChunkAddress,
    provider: Arc<dyn SwarmChunkProvider>,
    shared: Arc<Mutex<HashMap<ChunkAddress, AnyChunk>>>,
) -> Result<(), JsValue> {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    use futures::stream::FuturesUnordered;

    // A pipelined node reaches the network earlier than under a level walk, so a
    // chunk's whole candidate race can fail on transient transport congestion
    // rather than chunk absence; retry a few times with backoff before leaving it
    // for the joiner's own ordered retrieval.
    const MAX_CHUNK_RETRIES: u32 = 6;
    const RETRY_BACKOFF_STEP: std::time::Duration = std::time::Duration::from_millis(150);

    let mut seen: HashSet<ChunkAddress> = HashSet::new();
    seen.insert(root);

    let limit = prefetch_concurrency().max(1);
    // Min-heap on depth: the shallowest (ancestor) node pops first, so the
    // intermediate spine is fetched ahead of its leaves and the tree is
    // discovered fast, while the global in-flight pool stays full across what
    // used to be level boundaries. No per-level barrier: a leaf is dispatched the
    // moment its parent intermediate resolves, not after the whole intermediate
    // level drains, so the fan-out ramps to full width without waiting on the
    // slowest intermediate.
    let mut pending: BinaryHeap<Reverse<(u32, ChunkAddress)>> = BinaryHeap::new();
    pending.push(Reverse((0, root)));
    let mut in_flight = FuturesUnordered::new();

    let spawn = |addr: ChunkAddress, depth: u32, attempt: u32| {
        let provider = Arc::clone(&provider);
        let cached = shared.lock().expect("cache mutex").get(&addr).cloned();
        async move {
            if attempt > 0 {
                vertex_tasks::time::sleep(RETRY_BACKOFF_STEP * attempt).await;
            }
            let outcome = match cached {
                Some(chunk) => Ok(chunk),
                None => {
                    let r = provider.retrieve_chunk(&addr).await.map(|r| r.chunk);
                    // Cede between resolved retrievals so the pool drain does not
                    // monopolise the single browser thread and starve the swarm
                    // run loop that feeds the next responses.
                    maybe_yield_to_event_loop().await;
                    r
                }
            };
            (addr, depth, attempt, outcome)
        }
    };

    // Prime the pool, then refill from `pending` as slots free.
    while in_flight.len() < limit {
        match pending.pop() {
            Some(Reverse((depth, addr))) => in_flight.push(spawn(addr, depth, 0)),
            None => break,
        }
    }

    let mut skipped = 0usize;
    while let Some((addr, depth, attempt, outcome)) = in_flight.next().await {
        let chunk = match outcome {
            Ok(chunk) => chunk,
            Err(_) if attempt + 1 < MAX_CHUNK_RETRIES => {
                in_flight.push(spawn(addr, depth, attempt + 1));
                continue;
            }
            // A node that exhausts its retries is left for the joiner's own
            // ordered retrieval; never abort the whole download on one prefetch
            // miss. Skipping an intermediate forgoes warming its subtree, which
            // the joiner then re-fetches and walks.
            Err(_) => {
                skipped += 1;
                while in_flight.len() < limit {
                    match pending.pop() {
                        Some(Reverse((depth, addr))) => in_flight.push(spawn(addr, depth, 0)),
                        None => break,
                    }
                }
                continue;
            }
        };
        if chunk.span() > DEFAULT_BODY_SIZE as u64 {
            for child in parse_child_refs(chunk.data())? {
                if seen.insert(child) {
                    pending.push(Reverse((depth + 1, child)));
                }
            }
        }
        shared
            .lock()
            .expect("cache mutex")
            .insert(*chunk.address(), chunk);
        // Refill freed slots, shallowest-first, from the work queue.
        while in_flight.len() < limit {
            match pending.pop() {
                Some(Reverse((depth, addr))) => in_flight.push(spawn(addr, depth, 0)),
                None => break,
            }
        }
    }

    if skipped > 0 {
        tracing::info!("prefetch-skipped skipped={skipped} (pipelined into shared)");
    }
    Ok(())
}

/// Parse an intermediate node's body as packed 32-byte child chunk addresses.
fn parse_child_refs(body: &[u8]) -> Result<Vec<ChunkAddress>, JsValue> {
    if !body.len().is_multiple_of(REF_SIZE) {
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
