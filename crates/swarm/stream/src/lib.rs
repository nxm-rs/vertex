//! Memory-bounded streaming multi-chunk get/put pipelines.
//!
//! The single-chunk [`SwarmChunkProvider::retrieve_chunk`] and
//! [`SwarmChunkSender`] entry points serve one address at a time. A file is
//! thousands of chunks, so a caller that wants a whole manifest has two bad
//! options on its own: issue them one by one and eat the round-trip latency, or
//! spawn one future per address and let an unbounded fan-out blow the heap on a
//! slow or hostile consumer. This module is the third option: an ordered stream
//! that prefetches ahead of the consumer up to a window expressed in *bytes*,
//! never more, and stops pulling new work the instant the consumer stops
//! draining.
//!
//! Both pipelines are plain [`Stream`]s, FFI-agnostic and `Send` where the
//! underlying provider is. The bound lives in Rust: the stream only issues a new
//! request when its in-flight byte reservation plus the next request fits the
//! window, and it only advances when polled. A host that polls slowly (a Dart
//! `StreamSink` whose listener is paused, a browser `ReadableStream` under
//! backpressure) transitively pauses the network reads, so memory stays flat at
//! roughly `window_bytes` no matter how long the address list is.
//!
//! The crate is the transport-agnostic core that every chunk-bulk consumer
//! shares: the native FFI adapter in `vertex-ffi` wraps these streams as Dart
//! sinks, the browser adapter in the [`wasm`] module surfaces them to
//! JavaScript as async iterators, and the future gRPC chunk service streams
//! the same items over the wire. The pipelines depend only on the
//! [`SwarmChunkProvider`] and [`SwarmChunkSender`] traits, never on a node
//! internal type, so the same combinator serves all three.
//!
//! Downloads deliver [`VerifiedChunk`] only: every chunk is proven to answer the
//! address that requested it before it leaves the stream, so a peer that returns
//! the wrong bytes for an address surfaces as an error item, never as a chunk a
//! consumer might trust. The stamp on a delivery is optional (a storer may omit
//! it), so the verified item carries an `Option<Stamp>`.
//!
//! The in-window concurrency is built on the per-request outbound futures: each
//! retrieval or push is a self-contained future correlated by its own substream,
//! so racing many addresses at once never aliases response state. Output order
//! always matches input order via [`FuturesOrdered`], independent of which
//! request completes first.

#[cfg(target_arch = "wasm32")]
pub mod wasm;

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::StreamExt;
use futures::stream::{FuturesOrdered, Stream};
use nectar_primitives::{AnyChunk, ChunkAddress};
use vertex_swarm_api::{
    PushReceipt, Stamp, StampedChunk, SwarmChunkProvider, SwarmChunkSender, SwarmError, SwarmResult,
};
use vertex_tasks::MaybeSendBoxFuture;

/// A downloaded chunk proven to answer the address that requested it.
///
/// The chunk's address is derived from its own bytes (the BMT hash for a content
/// chunk, owner plus signature for a single-owner chunk), and the download stream
/// proves that address equals the requested one before yielding the item, so a
/// peer that returns the wrong bytes surfaces as an error rather than a trusted
/// chunk.
///
/// The stamp is optional: a storer answers a retrieval with the chunk bytes and
/// may omit the stamp from the delivery, which is never re-read on the download
/// path. Address integrity does not depend on the stamp, so a stampless delivery
/// is still a fully verified chunk.
#[derive(Debug, Clone)]
pub struct VerifiedChunk {
    chunk: AnyChunk,
    stamp: Option<Stamp>,
}

impl VerifiedChunk {
    /// The verified chunk.
    #[must_use]
    pub fn chunk(&self) -> &AnyChunk {
        &self.chunk
    }

    /// The chunk's address (delegates to the chunk).
    #[must_use]
    pub fn address(&self) -> &ChunkAddress {
        self.chunk.address()
    }

    /// The postage stamp the responder attached, if any.
    #[must_use]
    pub fn stamp(&self) -> Option<&Stamp> {
        self.stamp.as_ref()
    }

    /// Split into the chunk and its optional stamp.
    #[must_use]
    pub fn into_parts(self) -> (AnyChunk, Option<Stamp>) {
        (self.chunk, self.stamp)
    }
}

/// Maximum wire size of a single Swarm chunk: an 8-byte span plus a 4096-byte
/// body. The download pipeline reserves this per in-flight retrieval because a
/// chunk's true size is unknown until it arrives; the upload pipeline reserves
/// each chunk's actual encoded size instead.
pub const MAX_CHUNK_BYTES: usize = 8 + nectar_primitives::DEFAULT_BODY_SIZE;

/// Bound on how much chunk payload a streaming pipeline keeps in flight.
///
/// The window is the memory ceiling, expressed in bytes rather than a chunk
/// count so a caller sizes it against a real budget instead of guessing a chunk
/// size. `max_concurrency` is a hard cap on simultaneous in-flight requests that
/// applies on top of the byte budget, so a generous window cannot fan out to
/// thousands of sockets at once.
///
/// At least one request is always admitted even if `window_bytes` is smaller
/// than a single chunk, so a tiny window degrades to one-at-a-time streaming
/// rather than deadlocking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamConfig {
    /// Soft byte ceiling on outstanding (in-flight plus buffered) payload.
    pub window_bytes: usize,
    /// Hard cap on simultaneous in-flight requests.
    pub max_concurrency: usize,
}

impl StreamConfig {
    /// A 1 MiB window with up to 32 concurrent requests.
    ///
    /// Sized for a typical mobile or browser client: enough prefetch to hide
    /// round-trip latency across a manifest without letting a slow consumer or a
    /// long address list grow the heap.
    pub const DEFAULT: Self = Self {
        window_bytes: 1 << 20,
        max_concurrency: 32,
    };

    /// Build a config, clamping both knobs to sane minimums.
    ///
    /// A zero window or zero concurrency would otherwise deadlock the pipeline;
    /// both are raised to one so the stream always makes forward progress.
    #[must_use]
    pub fn new(window_bytes: usize, max_concurrency: usize) -> Self {
        Self {
            window_bytes: window_bytes.max(1),
            max_concurrency: max_concurrency.max(1),
        }
    }

    /// Concurrency the byte window permits for download requests, where each
    /// in-flight retrieval reserves [`MAX_CHUNK_BYTES`].
    ///
    /// Always at least one so a window narrower than a chunk still streams.
    fn download_concurrency(self) -> usize {
        let by_bytes = (self.window_bytes / MAX_CHUNK_BYTES).max(1);
        by_bytes.min(self.max_concurrency)
    }
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Stream of verified chunks retrieved for an ordered list of addresses.
///
/// Yields one [`SwarmResult`] per input address, in input order. A retrieval
/// that fails or returns bytes that do not answer the requested address yields
/// an [`Err`] in that slot; the stream continues with the remaining addresses
/// rather than aborting the whole download.
///
/// The pipeline keeps at most [`StreamConfig::download_concurrency`] retrievals
/// in flight and admits a new one only as the consumer drains a completed slot,
/// so outstanding payload stays within the configured byte window.
pub fn get_stream<P>(
    provider: P,
    addresses: impl IntoIterator<Item = ChunkAddress>,
    config: StreamConfig,
) -> GetStream<P>
where
    P: SwarmChunkProvider + Clone + 'static,
{
    GetStream {
        provider,
        pending: addresses.into_iter().collect(),
        in_flight: FuturesOrdered::new(),
        limit: config.download_concurrency(),
    }
}

/// Stream returned by [`get_stream`].
///
/// Holds the provider, the queue of addresses not yet requested, and the ordered
/// set of in-flight retrievals. Each poll first tops the in-flight set up to the
/// concurrency limit (prefetch), then yields the next completed retrieval in
/// input order.
#[must_use = "a stream does nothing unless polled"]
pub struct GetStream<P> {
    provider: P,
    pending: VecDeque<ChunkAddress>,
    in_flight: FuturesOrdered<MaybeSendBoxFuture<SwarmResult<VerifiedChunk>>>,
    limit: usize,
}

/// Retrieve one chunk and prove it answers `address`.
///
/// The provider establishes content integrity (the chunk hashes to its own
/// address); this proves that address equals the requested one, turning a
/// delivery into a [`VerifiedChunk`]. A mismatch is treated as invalid data from
/// the peer, not a value to hand back. The stamp is carried through unchanged: it
/// is optional, and address integrity does not depend on it.
async fn retrieve_verified<P>(provider: P, address: ChunkAddress) -> SwarmResult<VerifiedChunk>
where
    P: SwarmChunkProvider,
{
    let result = provider.retrieve_chunk(&address).await?;
    if *result.chunk.address() != address {
        return Err(SwarmError::InvalidChunk {
            address: Some(address),
            reason: "retrieved chunk does not answer the requested address".to_string(),
        });
    }
    Ok(VerifiedChunk {
        chunk: result.chunk,
        stamp: result.stamp,
    })
}

impl<P> GetStream<P>
where
    P: SwarmChunkProvider + Clone + 'static,
{
    /// Admit pending addresses until the in-flight set hits the concurrency
    /// limit or the queue drains. This is the prefetch: it runs ahead of the
    /// consumer up to the window the limit encodes.
    fn refill(&mut self) {
        while self.in_flight.len() < self.limit {
            let Some(address) = self.pending.pop_front() else {
                break;
            };
            let provider = self.provider.clone();
            self.in_flight
                .push_back(Box::pin(retrieve_verified(provider, address)));
        }
    }
}

impl<P> Stream for GetStream<P>
where
    P: SwarmChunkProvider + Clone + Unpin + 'static,
{
    type Item = SwarmResult<VerifiedChunk>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        // Prefetch first: top the in-flight window up before polling it, so a
        // drained slot is immediately refilled from the pending queue.
        this.refill();

        match this.in_flight.poll_next_unpin(cx) {
            // A slot completed. Refill so the next poll already has work queued,
            // then hand the result out. This is where backpressure lives: the
            // refill above admits exactly one replacement per drained slot, so a
            // consumer that stops polling stops all new requests.
            Poll::Ready(Some(item)) => {
                this.refill();
                Poll::Ready(Some(item))
            }
            // No in-flight work and nothing pending: the stream is done.
            Poll::Ready(None) if this.pending.is_empty() => Poll::Ready(None),
            // The in-flight set is momentarily empty but addresses remain (the
            // limit was zero-length this tick). Refill admitted them above;
            // re-poll on the next wake.
            Poll::Ready(None) => {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.pending.len() + self.in_flight.len();
        (remaining, Some(remaining))
    }
}

/// Stream of push receipts for an ordered list of chunks to upload.
///
/// Yields one [`SwarmResult`] per input chunk, in input order. The byte window
/// here is exact: each chunk reserves its own encoded size against
/// `window_bytes`, and a new push is admitted only when the next chunk fits the
/// remaining budget (or when nothing is in flight, so an oversized chunk still
/// makes progress one at a time). The same `max_concurrency` cap applies.
///
/// `chunks` is consumed lazily: the pipeline pulls the next [`StampedChunk`] from
/// the iterator only when it admits a push, so a caller that builds each chunk on
/// demand (reconstructing from raw bytes inside the iterator) holds at most a
/// window's worth of materialized chunks at once, not the whole list. A caller
/// that passes an already-materialized `Vec` still owns that `Vec`, but the
/// pipeline adds no second copy on top of it.
pub fn put_stream<S, I>(sender: S, chunks: I, config: StreamConfig) -> PutStream<S>
where
    S: SwarmChunkSender + Clone + 'static,
    I: IntoIterator<Item = StampedChunk>,
    I::IntoIter: MaybeSendIter + 'static,
{
    try_put_stream(sender, chunks.into_iter().map(Ok), config)
}

/// Stream of push receipts for an ordered feed of fallibly-produced chunks.
///
/// The same byte-bounded pipeline as [`put_stream`], but the feed yields
/// `SwarmResult<StampedChunk>` so a caller that builds chunks on demand can
/// surface a per-chunk build failure (a byte/address mismatch, a bad stamp) as an
/// error in that chunk's output slot instead of aborting the whole upload. A feed
/// `Err` consumes no window budget and issues no push; it is yielded in order,
/// preserving the one-output-per-input contract.
pub fn try_put_stream<S, I>(sender: S, chunks: I, config: StreamConfig) -> PutStream<S>
where
    S: SwarmChunkSender + Clone + 'static,
    I: IntoIterator<Item = SwarmResult<StampedChunk>>,
    I::IntoIter: MaybeSendIter + 'static,
{
    PutStream {
        sender,
        pending: BoxedChunks::box_chunks(chunks.into_iter()).peekable(),
        in_flight: FuturesOrdered::new(),
        reserved_bytes: 0,
        window_bytes: config.window_bytes,
        limit: config.max_concurrency,
    }
}

/// Marker for the pending-chunk iterator's thread-safety requirement: `Send` on
/// native so [`PutStream`] stays `Send` (the FFI handle is driven across the
/// runtime and the tests spawn it), unconstrained on wasm. Mirrors the native vs
/// wasm split of [`MaybeSendBoxFuture`]. Blanket-implemented, so callers never
/// name it.
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSendIter: Send {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send> MaybeSendIter for T {}
#[cfg(target_arch = "wasm32")]
pub trait MaybeSendIter {}
#[cfg(target_arch = "wasm32")]
impl<T> MaybeSendIter for T {}

/// Boxed pending-chunk feed. `Send` on native so [`PutStream`] stays `Send`;
/// not required `Send` on wasm.
#[cfg(not(target_arch = "wasm32"))]
type BoxedChunks = Box<dyn Iterator<Item = SwarmResult<StampedChunk>> + Send>;
#[cfg(target_arch = "wasm32")]
type BoxedChunks = Box<dyn Iterator<Item = SwarmResult<StampedChunk>>>;

/// Box a feed iterator into [`BoxedChunks`] with the right `Send`-ness per target.
trait BoxedChunksExt {
    fn box_chunks<I: Iterator<Item = SwarmResult<StampedChunk>> + MaybeSendIter + 'static>(
        iter: I,
    ) -> Self;
}

impl BoxedChunksExt for BoxedChunks {
    fn box_chunks<I: Iterator<Item = SwarmResult<StampedChunk>> + MaybeSendIter + 'static>(
        iter: I,
    ) -> Self {
        Box::new(iter)
    }
}

/// Iterator the upload pipeline pulls chunks from. Boxed so the stream type is
/// independent of how the caller produces chunks (an eager `Vec` or a lazy
/// reconstruct-on-demand feed), and peekable so admission can size the next chunk
/// against the window before consuming it.
type PendingChunks = std::iter::Peekable<BoxedChunks>;

/// Stream returned by [`put_stream`].
///
/// Tracks the bytes reserved by in-flight pushes so admission is by real payload
/// size, not an estimate: a chunk is admitted only when its encoded size fits the
/// remaining window (or the in-flight set is empty). Pending chunks are pulled
/// lazily from the source iterator, so only admitted chunks are materialized.
#[must_use = "a stream does nothing unless polled"]
pub struct PutStream<S> {
    sender: S,
    pending: PendingChunks,
    in_flight: FuturesOrdered<MaybeSendBoxFuture<(usize, SwarmResult<PushReceipt>)>>,
    reserved_bytes: usize,
    window_bytes: usize,
    limit: usize,
}

/// Push one chunk, carrying its reserved byte size out alongside the receipt so
/// the stream can release the reservation when the push completes.
async fn push_chunk<S>(
    sender: S,
    chunk: StampedChunk,
    bytes: usize,
) -> (usize, SwarmResult<PushReceipt>)
where
    S: SwarmChunkSender,
{
    let receipt = sender.send_chunk(chunk).await;
    (bytes, receipt)
}

impl<S> PutStream<S>
where
    S: SwarmChunkSender + Clone + 'static,
{
    /// Admit pending chunks while they fit the byte window and the concurrency
    /// cap. A chunk larger than the whole window is still admitted when nothing
    /// is in flight, so an oversized upload makes progress one at a time rather
    /// than deadlocking.
    fn refill(&mut self) {
        while self.in_flight.len() < self.limit {
            let bytes = match self.pending.peek() {
                None => break,
                // A feed error reserves no budget and issues no push: it is
                // yielded in order from a ready future so the slot is preserved.
                // Admit it whenever there is a free concurrency slot.
                Some(Err(_)) => {
                    let Some(Err(error)) = self.pending.next() else {
                        break;
                    };
                    self.in_flight
                        .push_back(Box::pin(async move { (0, Err(error)) }));
                    continue;
                }
                Some(Ok(chunk)) => {
                    let bytes = chunk.chunk().size();
                    let fits = self.reserved_bytes + bytes <= self.window_bytes;
                    let nothing_in_flight = self.in_flight.is_empty();
                    if !fits && !nothing_in_flight {
                        break;
                    }
                    bytes
                }
            };

            // Admission decided against the peeked chunk above; take that same
            // one now. The peek borrow ended before this `next`, and the body
            // never yields, so the peeked chunk is exactly the one consumed.
            let Some(Ok(chunk)) = self.pending.next() else {
                break;
            };
            self.reserved_bytes += bytes;
            let sender = self.sender.clone();
            self.in_flight
                .push_back(Box::pin(push_chunk(sender, chunk, bytes)));
        }
    }
}

impl<S> Stream for PutStream<S>
where
    S: SwarmChunkSender + Clone + Unpin + 'static,
{
    type Item = SwarmResult<PushReceipt>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        this.refill();

        match this.in_flight.poll_next_unpin(cx) {
            Poll::Ready(Some((bytes, result))) => {
                // Release the completed push's reservation, then refill so the
                // freed budget admits the next chunk. Saturating because the
                // reservation can never legitimately exceed what was reserved,
                // but underflow on a logic slip must not panic the pipeline.
                this.reserved_bytes = this.reserved_bytes.saturating_sub(bytes);
                this.refill();
                Poll::Ready(Some(result))
            }
            Poll::Ready(None) if this.pending.peek().is_none() => Poll::Ready(None),
            Poll::Ready(None) => {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        // The pending source is a lazy iterator, so only its declared bound is
        // known; the in-flight set is exact. The lower bound counts in-flight
        // plus whatever the iterator guarantees remain.
        let (pending_lo, pending_hi) = self.pending.size_hint();
        let lower = pending_lo + self.in_flight.len();
        let upper = pending_hi.map(|hi| hi + self.in_flight.len());
        (lower, upper)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use alloy_primitives::{B256, Signature};
    use async_trait::async_trait;
    use nectar_primitives::{AnyChunk, ContentChunk, Nonce};
    use vertex_swarm_api::{
        ChunkRetrievalResult, OverlayAddress, Stamp, StorageRadius, SwarmChunkProvider,
        SwarmChunkSender,
    };

    use super::*;

    fn test_stamp() -> Stamp {
        let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
        Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, sig)
    }

    /// Build a content chunk from distinct bytes so each test chunk has a unique
    /// address.
    fn chunk_for(seed: u8) -> StampedChunk {
        let payload = vec![seed; 64];
        let chunk = ContentChunk::new(payload).expect("valid content chunk");
        StampedChunk::new(AnyChunk::Content(chunk), test_stamp())
    }

    fn receipt() -> PushReceipt {
        PushReceipt {
            storer: OverlayAddress::from([7u8; 32]),
            signature: *test_stamp().signature(),
            nonce: Nonce::from([9u8; 32]),
            storage_radius: StorageRadius::ZERO,
        }
    }

    /// Provider that serves a fixed map of address -> chunk and counts the
    /// maximum number of simultaneously in-flight retrievals it observed.
    #[derive(Clone)]
    struct MapProvider {
        chunks: Arc<std::collections::HashMap<ChunkAddress, StampedChunk>>,
        in_flight: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
        gate: Arc<tokio::sync::Semaphore>,
    }

    impl MapProvider {
        fn new(chunks: Vec<StampedChunk>) -> Self {
            let map = chunks
                .into_iter()
                .map(|c| (*c.address(), c))
                .collect::<std::collections::HashMap<_, _>>();
            Self {
                chunks: Arc::new(map),
                in_flight: Arc::new(AtomicUsize::new(0)),
                peak: Arc::new(AtomicUsize::new(0)),
                gate: Arc::new(tokio::sync::Semaphore::new(usize::MAX >> 4)),
            }
        }

        /// A provider whose retrievals block until `permits` are added, so a test
        /// can observe the steady-state in-flight count.
        fn gated(chunks: Vec<StampedChunk>) -> Self {
            let mut p = Self::new(chunks);
            p.gate = Arc::new(tokio::sync::Semaphore::new(0));
            p
        }
    }

    #[async_trait]
    impl SwarmChunkProvider for MapProvider {
        async fn retrieve_chunk(
            &self,
            address: &ChunkAddress,
        ) -> SwarmResult<ChunkRetrievalResult> {
            let cur = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(cur, Ordering::SeqCst);
            let permit = self.gate.acquire().await.unwrap();
            permit.forget();
            self.in_flight.fetch_sub(1, Ordering::SeqCst);

            match self.chunks.get(address) {
                Some(chunk) => {
                    let (chunk, stamp) = chunk.clone().into_parts();
                    Ok(ChunkRetrievalResult {
                        chunk,
                        stamp: Some(stamp),
                        served_by: OverlayAddress::from([1u8; 32]),
                    })
                }
                None => Err(SwarmError::ChunkNotFound { address: *address }),
            }
        }

        fn has_chunk(&self, _address: &ChunkAddress) -> bool {
            false
        }
    }

    /// Provider that always returns a chunk for a *different* address than the
    /// one requested, modelling a peer that answers with the wrong bytes.
    #[derive(Clone)]
    struct WrongChunkProvider {
        chunk: StampedChunk,
    }

    #[async_trait]
    impl SwarmChunkProvider for WrongChunkProvider {
        async fn retrieve_chunk(
            &self,
            _address: &ChunkAddress,
        ) -> SwarmResult<ChunkRetrievalResult> {
            let (chunk, stamp) = self.chunk.clone().into_parts();
            Ok(ChunkRetrievalResult {
                chunk,
                stamp: Some(stamp),
                served_by: OverlayAddress::from([2u8; 32]),
            })
        }

        fn has_chunk(&self, _address: &ChunkAddress) -> bool {
            false
        }
    }

    /// Sender that records every chunk it accepts and tracks peak concurrency.
    #[derive(Clone)]
    struct RecordingSender {
        accepted: Arc<std::sync::Mutex<Vec<ChunkAddress>>>,
        in_flight: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
        gate: Arc<tokio::sync::Semaphore>,
        fail_on: Option<ChunkAddress>,
    }

    impl RecordingSender {
        fn new() -> Self {
            Self {
                accepted: Arc::new(std::sync::Mutex::new(Vec::new())),
                in_flight: Arc::new(AtomicUsize::new(0)),
                peak: Arc::new(AtomicUsize::new(0)),
                gate: Arc::new(tokio::sync::Semaphore::new(usize::MAX >> 4)),
                fail_on: None,
            }
        }

        fn gated() -> Self {
            let mut s = Self::new();
            s.gate = Arc::new(tokio::sync::Semaphore::new(0));
            s
        }
    }

    #[async_trait]
    impl SwarmChunkSender for RecordingSender {
        async fn send_chunk_unchecked(&self, chunk: StampedChunk) -> SwarmResult<PushReceipt> {
            self.send_chunk(chunk).await
        }

        async fn send_chunk(&self, chunk: StampedChunk) -> SwarmResult<PushReceipt> {
            let cur = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(cur, Ordering::SeqCst);
            let permit = self.gate.acquire().await.unwrap();
            permit.forget();
            self.in_flight.fetch_sub(1, Ordering::SeqCst);

            let address = *chunk.address();
            if Some(address) == self.fail_on {
                return Err(SwarmError::NoStorer {
                    chunk_address: address,
                });
            }
            self.accepted.lock().unwrap().push(address);
            Ok(receipt())
        }
    }

    #[tokio::test]
    async fn get_stream_yields_chunks_in_address_order() {
        let chunks: Vec<_> = (0..8).map(chunk_for).collect();
        let addresses: Vec<_> = chunks.iter().map(|c| *c.address()).collect();
        let provider = MapProvider::new(chunks);

        let stream = get_stream(provider, addresses.clone(), StreamConfig::new(1 << 20, 4));
        let results: Vec<_> = stream.collect().await;

        assert_eq!(results.len(), addresses.len());
        for (result, expected) in results.into_iter().zip(addresses) {
            let verified = result.expect("retrieval succeeds");
            assert_eq!(*verified.address(), expected);
        }
    }

    #[tokio::test]
    async fn get_stream_caps_concurrency_at_the_byte_window() {
        // A window of 3 max-chunks permits exactly 3 concurrent retrievals even
        // though max_concurrency is higher and many addresses are pending.
        let chunks: Vec<_> = (0..16).map(chunk_for).collect();
        let addresses: Vec<_> = chunks.iter().map(|c| *c.address()).collect();
        let provider = MapProvider::gated(chunks);
        let peak = Arc::clone(&provider.peak);
        let gate = Arc::clone(&provider.gate);
        let in_flight = Arc::clone(&provider.in_flight);

        let window = 3 * MAX_CHUNK_BYTES;
        let mut stream = get_stream(provider, addresses, StreamConfig::new(window, 64));

        // Drive the stream forward without consuming: poll it so it fills the
        // in-flight window, then let the gated retrievals settle.
        let driver = tokio::spawn(async move {
            let _ = stream.next().await;
            stream
        });

        // Wait until the pipeline has saturated its window.
        loop {
            if in_flight.load(Ordering::SeqCst) >= 3 {
                break;
            }
            tokio::task::yield_now().await;
        }
        // Give any erroneous extra retrieval a chance to register.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(peak.load(Ordering::SeqCst) <= 3, "window must cap fan-out");

        // Release everything so the driver can finish.
        gate.add_permits(1 << 20);
        let _ = driver.await.unwrap();
    }

    #[tokio::test]
    async fn get_stream_window_smaller_than_chunk_still_streams_one_at_a_time() {
        let chunks: Vec<_> = (0..4).map(chunk_for).collect();
        let addresses: Vec<_> = chunks.iter().map(|c| *c.address()).collect();
        let provider = MapProvider::new(chunks);

        // window_bytes far below one chunk: concurrency must clamp to 1.
        let stream = get_stream(provider, addresses.clone(), StreamConfig::new(1, 64));
        let results: Vec<_> = stream.collect().await;
        assert_eq!(results.len(), addresses.len());
        assert!(results.into_iter().all(|r| r.is_ok()));
    }

    #[tokio::test]
    async fn get_stream_surfaces_missing_chunk_as_error_item_and_continues() {
        let present: Vec<_> = (0..3).map(chunk_for).collect();
        let mut addresses: Vec<_> = present.iter().map(|c| *c.address()).collect();
        // Insert a never-stored address in the middle.
        let missing = ChunkAddress::new([0xfe; 32]);
        addresses.insert(1, missing);
        let provider = MapProvider::new(present);

        let stream = get_stream(provider, addresses.clone(), StreamConfig::new(1 << 20, 4));
        let results: Vec<_> = stream.collect().await;

        assert_eq!(results.len(), addresses.len());
        assert!(results[0].is_ok());
        assert!(matches!(results[1], Err(SwarmError::ChunkNotFound { .. })));
        assert!(results[2].is_ok());
        assert!(results[3].is_ok());
    }

    #[tokio::test]
    async fn get_stream_rejects_wrong_chunk_for_address() {
        // The peer answers every request with a fixed chunk; only the request
        // for that chunk's own address may verify, the rest must error.
        let served = chunk_for(0);
        let served_address = *served.address();
        let other = *chunk_for(99).address();
        let provider = WrongChunkProvider { chunk: served };

        let stream = get_stream(
            provider,
            vec![served_address, other],
            StreamConfig::new(1 << 20, 4),
        );
        let results: Vec<_> = stream.collect().await;

        assert!(
            results[0].is_ok(),
            "address that matches the bytes verifies"
        );
        assert!(
            matches!(results[1], Err(SwarmError::InvalidChunk { .. })),
            "wrong bytes for an address must be rejected: {:?}",
            results[1]
        );
    }

    #[tokio::test]
    async fn get_stream_empty_input_terminates() {
        let provider = MapProvider::new(vec![]);
        let stream = get_stream(provider, Vec::new(), StreamConfig::DEFAULT);
        let results: Vec<_> = stream.collect().await;
        assert!(results.is_empty());
    }

    /// A provider answering with the chunk but no stamp (a storer omits it).
    #[derive(Clone)]
    struct StamplessProvider {
        chunk: AnyChunk,
    }

    #[async_trait]
    impl SwarmChunkProvider for StamplessProvider {
        async fn retrieve_chunk(
            &self,
            _address: &ChunkAddress,
        ) -> SwarmResult<ChunkRetrievalResult> {
            Ok(ChunkRetrievalResult {
                chunk: self.chunk.clone(),
                stamp: None,
                served_by: OverlayAddress::from([3u8; 32]),
            })
        }

        fn has_chunk(&self, _address: &ChunkAddress) -> bool {
            false
        }
    }

    /// A stampless delivery still verifies against its address and is yielded as
    /// a `VerifiedChunk` carrying no stamp. This is the bee-interop download path.
    #[tokio::test]
    async fn get_stream_accepts_stampless_delivery() {
        let chunk = chunk_for(5).into_parts().0;
        let address = *chunk.address();
        let provider = StamplessProvider { chunk };

        let stream = get_stream(provider, vec![address], StreamConfig::new(1 << 20, 4));
        let mut results: Vec<_> = stream.collect().await;
        let verified = results.remove(0).expect("a stampless delivery verifies");
        assert_eq!(*verified.address(), address);
        assert!(verified.stamp().is_none(), "no stamp is carried through");
    }

    #[tokio::test]
    async fn put_stream_uploads_all_chunks_in_order() {
        let chunks: Vec<_> = (0..8).map(chunk_for).collect();
        let addresses: Vec<_> = chunks.iter().map(|c| *c.address()).collect();
        let sender = RecordingSender::new();
        let accepted = Arc::clone(&sender.accepted);

        let stream = put_stream(sender, chunks, StreamConfig::new(1 << 20, 4));
        let results: Vec<_> = stream.collect().await;

        assert_eq!(results.len(), addresses.len());
        assert!(results.into_iter().all(|r| r.is_ok()));
        // Every chunk was accepted exactly once.
        let accepted = accepted.lock().unwrap();
        assert_eq!(accepted.len(), addresses.len());
        for address in &addresses {
            assert!(accepted.contains(address));
        }
    }

    #[tokio::test]
    async fn put_stream_caps_in_flight_bytes_at_the_window() {
        // Each chunk encodes to 8 + 64 = 72 bytes; a window of 3 chunks worth
        // permits 3 concurrent pushes.
        let chunks: Vec<_> = (0..16).map(chunk_for).collect();
        let chunk_bytes = chunks[0].chunk().size();
        let sender = RecordingSender::gated();
        let peak = Arc::clone(&sender.peak);
        let gate = Arc::clone(&sender.gate);
        let in_flight = Arc::clone(&sender.in_flight);

        let window = 3 * chunk_bytes;
        let mut stream = put_stream(sender, chunks, StreamConfig::new(window, 64));
        let driver = tokio::spawn(async move {
            let _ = stream.next().await;
            stream
        });

        loop {
            if in_flight.load(Ordering::SeqCst) >= 3 {
                break;
            }
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            peak.load(Ordering::SeqCst) <= 3,
            "byte window must cap pushes"
        );

        gate.add_permits(1 << 20);
        let _ = driver.await.unwrap();
    }

    #[tokio::test]
    async fn put_stream_materializes_chunks_lazily_within_the_window() {
        // The upload pipeline must pull from its source iterator only as it
        // admits pushes, so a caller that reconstructs each chunk on demand keeps
        // at most a window's worth of chunks resident, not the whole list. A
        // counting iterator over a long list, with a gated sender and a 3-chunk
        // window, must never have produced more than the in-flight cap by the
        // time the window saturates.
        let materialized = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&materialized);
        let chunk_bytes = chunk_for(0).chunk().size();
        let source = (0..1024).map(move |i| {
            counter.fetch_add(1, Ordering::SeqCst);
            chunk_for((i % 251) as u8)
        });

        let sender = RecordingSender::gated();
        let gate = Arc::clone(&sender.gate);
        let in_flight = Arc::clone(&sender.in_flight);

        let window = 3 * chunk_bytes;
        let mut stream = put_stream(sender, source, StreamConfig::new(window, 64));
        let driver = tokio::spawn(async move {
            let _ = stream.next().await;
            stream
        });

        loop {
            if in_flight.load(Ordering::SeqCst) >= 3 {
                break;
            }
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        // Only the admitted (in-flight) chunks plus at most one peeked-but-not-
        // admitted chunk are ever pulled from the source. Far below the 1024 in
        // the list: the heap holds the window, not the input.
        assert!(
            materialized.load(Ordering::SeqCst) <= 4,
            "lazy: at most window+1 chunks materialized, got {}",
            materialized.load(Ordering::SeqCst)
        );

        gate.add_permits(1 << 20);
        let _ = driver.await.unwrap();
    }

    #[tokio::test]
    async fn put_stream_oversized_chunk_makes_progress_one_at_a_time() {
        // A window smaller than a single chunk must not deadlock: the empty
        // in-flight set admits one chunk regardless of the window.
        let chunks: Vec<_> = (0..4).map(chunk_for).collect();
        let sender = RecordingSender::new();
        let accepted = Arc::clone(&sender.accepted);

        let stream = put_stream(sender, chunks, StreamConfig::new(1, 64));
        let results: Vec<_> = stream.collect().await;

        assert_eq!(results.len(), 4);
        assert!(results.into_iter().all(|r| r.is_ok()));
        assert_eq!(accepted.lock().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn put_stream_surfaces_failed_push_and_continues() {
        let chunks: Vec<_> = (0..3).map(chunk_for).collect();
        let mut sender = RecordingSender::new();
        sender.fail_on = Some(*chunks[1].address());
        let accepted = Arc::clone(&sender.accepted);

        let stream = put_stream(sender, chunks, StreamConfig::new(1 << 20, 4));
        let results: Vec<_> = stream.collect().await;

        assert_eq!(results.len(), 3);
        assert!(results[0].is_ok());
        assert!(matches!(results[1], Err(SwarmError::NoStorer { .. })));
        assert!(results[2].is_ok());
        // The failed chunk was never recorded as accepted.
        assert_eq!(accepted.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn try_put_stream_surfaces_feed_error_in_order_and_continues() {
        // A fallible feed: the middle item fails to build. It must surface as an
        // error in its own slot, in order, without consuming a push, and the
        // surrounding chunks must still upload.
        let good0 = chunk_for(0);
        let good2 = chunk_for(2);
        let addr0 = *good0.address();
        let addr2 = *good2.address();
        let sender = RecordingSender::new();
        let accepted = Arc::clone(&sender.accepted);

        let feed = vec![
            Ok(good0),
            Err(SwarmError::InvalidChunk {
                address: None,
                reason: "bad bytes".to_string(),
            }),
            Ok(good2),
        ];
        let stream = try_put_stream(sender, feed, StreamConfig::new(1 << 20, 4));
        let results: Vec<_> = stream.collect().await;

        assert_eq!(results.len(), 3);
        assert!(results[0].is_ok());
        assert!(matches!(results[1], Err(SwarmError::InvalidChunk { .. })));
        assert!(results[2].is_ok());
        // Only the two well-formed chunks were ever pushed.
        let accepted = accepted.lock().unwrap();
        assert_eq!(accepted.len(), 2);
        assert!(accepted.contains(&addr0));
        assert!(accepted.contains(&addr2));
    }

    #[tokio::test]
    async fn put_stream_empty_input_terminates() {
        let sender = RecordingSender::new();
        let stream = put_stream(sender, Vec::new(), StreamConfig::DEFAULT);
        let results: Vec<_> = stream.collect().await;
        assert!(results.is_empty());
    }

    #[test]
    fn config_clamps_zero_to_one() {
        let cfg = StreamConfig::new(0, 0);
        assert_eq!(cfg.window_bytes, 1);
        assert_eq!(cfg.max_concurrency, 1);
        assert_eq!(cfg.download_concurrency(), 1);
    }

    #[test]
    fn download_concurrency_is_bounded_by_both_knobs() {
        // Byte budget allows 10 but max_concurrency caps at 4.
        let cfg = StreamConfig::new(10 * MAX_CHUNK_BYTES, 4);
        assert_eq!(cfg.download_concurrency(), 4);
        // Byte budget allows 2, below the concurrency cap.
        let cfg = StreamConfig::new(2 * MAX_CHUNK_BYTES, 64);
        assert_eq!(cfg.download_concurrency(), 2);
    }
}
