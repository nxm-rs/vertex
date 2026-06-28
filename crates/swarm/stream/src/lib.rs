//! Memory-bounded streaming multi-chunk get/put pipelines.
//!
//! Turns the single-chunk [`SwarmChunkProvider`] / [`SwarmChunkSender`] entry
//! points into [`Stream`]s over a whole address list, prefetching ahead of the
//! consumer up to a fixed number of chunks and stopping the instant it stops
//! draining. Backpressure is transitive: a slow consumer pauses the network
//! reads, so memory stays flat regardless of list length.
//!
//! Transport-agnostic core shared by the FFI, wasm, and gRPC chunk adapters;
//! depends only on the trait surface, never on a node internal type.
//!
//! Limiting is by chunk count, not bytes (a Swarm chunk is size-bounded);
//! byte/bandwidth limiting lives at the libp2p connection layer. Items are
//! yielded in completion order, so each carries its chunk address and reordering
//! is the consumer's job (the nectar `WindowedReader` is the reference
//! consumer). The concurrency cap bounds pipeline depth and memory, not
//! per-peer substream count; capping concurrent substreams to one peer lives in
//! the node retrieval layer. Downloads yield [`VerifiedChunk`] only: a chunk proven
//! to answer the requested address surfaces wrong bytes as an error item.

#[cfg(target_arch = "wasm32")]
pub mod wasm;

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::StreamExt;
use futures::stream::{FuturesUnordered, Stream};
use nectar_primitives::{AnyChunk, ChunkAddress};
use vertex_swarm_api::{
    OverlayAddress, PushReceipt, Stamp, StampedChunk, SwarmChunkProvider, SwarmChunkSender,
    SwarmError, SwarmResult,
};
// `Send` on native (so the streams stay `Send` for tonic), unbounded on wasm.
use vertex_tasks::{MaybeSend, MaybeSendBoxFuture, MaybeSendIter, MaybeSendStream};

/// A downloaded chunk proven to answer the address that requested it.
///
/// The stamp is optional: address integrity does not depend on it, so a
/// stampless delivery is still fully verified.
#[derive(Debug, Clone)]
pub struct VerifiedChunk {
    chunk: AnyChunk,
    stamp: Option<Stamp>,
    served_by: OverlayAddress,
}

impl VerifiedChunk {
    #[must_use]
    pub fn chunk(&self) -> &AnyChunk {
        &self.chunk
    }

    #[must_use]
    pub fn address(&self) -> &ChunkAddress {
        self.chunk.address()
    }

    #[must_use]
    pub fn stamp(&self) -> Option<&Stamp> {
        self.stamp.as_ref()
    }

    /// Overlay of the peer that served this chunk.
    #[must_use]
    pub fn served_by(&self) -> OverlayAddress {
        self.served_by
    }

    #[must_use]
    pub fn into_parts(self) -> (AnyChunk, Option<Stamp>) {
        (self.chunk, self.stamp)
    }
}

/// Maximum wire size of a single Swarm chunk: an 8-byte span plus a 4096-byte
/// body.
pub const MAX_CHUNK_BYTES: usize = 8 + nectar_primitives::DEFAULT_BODY_SIZE;

/// Sustained rate (chunks/s) a native node is assumed to serve from one bulk
/// download over forwarding retrieval: ~0.75 MiB/s of 4 KiB chunks.
const ASSUMED_SERVE_CHUNKS_PER_SEC: usize = 192;
/// Assumed mean wall-clock for one forwarding retrieval (kademlia hops + storer RTT).
const ASSUMED_RETRIEVAL_MILLIS: usize = 400;
/// Headroom over the bandwidth-delay product to absorb latency jitter.
const RETRIEVAL_BUFFER_PERCENT: usize = 25;

/// In-flight retrievals [`StreamConfig::NATIVE_DOWNLOAD`] keeps to saturate the
/// assumed serve rate over one retrieval latency, plus jitter headroom.
pub const NATIVE_DOWNLOAD_CONCURRENCY: usize =
    ASSUMED_SERVE_CHUNKS_PER_SEC * ASSUMED_RETRIEVAL_MILLIS * (100 + RETRIEVAL_BUFFER_PERCENT)
        / (1000 * 100);

/// How many chunks a streaming pipeline keeps in flight at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamConfig {
    /// Hard cap on chunks in flight at once.
    pub max_concurrency: usize,
}

impl StreamConfig {
    /// Up to 32 chunks in flight: a typical mobile or browser client.
    pub const DEFAULT: Self = Self {
        max_concurrency: 32,
    };

    /// High-throughput preset for a native node serving a bulk download:
    /// the bandwidth-delay product of [`NATIVE_DOWNLOAD_CONCURRENCY`].
    pub const NATIVE_DOWNLOAD: Self = Self {
        max_concurrency: NATIVE_DOWNLOAD_CONCURRENCY,
    };

    /// Clamps to at least one in flight so the stream always makes progress.
    #[must_use]
    pub fn new(max_concurrency: usize) -> Self {
        Self {
            max_concurrency: max_concurrency.max(1),
        }
    }
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A chunk address parse failed: the input was not exactly 32 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid chunk address: expected 32 bytes, got {got}")]
pub struct ParseAddressError {
    /// The length the caller actually supplied.
    pub got: usize,
}

/// Parse a 32-byte chunk address from raw bytes.
pub fn parse_address(bytes: &[u8]) -> Result<ChunkAddress, ParseAddressError> {
    ChunkAddress::from_slice(bytes).map_err(|_| ParseAddressError { got: bytes.len() })
}

/// Stream of verified chunks retrieved for a list of addresses.
///
/// Yields `(address, result)` per input address in completion order: each item
/// carries its address so the consumer correlates and reorders. A failed or
/// mismatched retrieval yields an [`Err`] for that address; the stream continues
/// rather than aborting the download.
///
/// The pipeline keeps at most [`StreamConfig::max_concurrency`] retrievals in
/// flight and admits a new one only as the consumer drains a completed slot.
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
        in_flight: FuturesUnordered::new(),
        // Clamp to >= 1 so a raw `StreamConfig { max_concurrency: 0 }` literal
        // never busy-loops without admitting work.
        limit: config.max_concurrency.max(1),
    }
}

pin_project_lite::pin_project! {
    /// Stream returned by [`get_stream`]. Each poll tops the in-flight set up to
    /// the concurrency limit, then yields the next completed retrieval.
    #[must_use = "a stream does nothing unless polled"]
    pub struct GetStream<P> {
        provider: P,
        pending: VecDeque<ChunkAddress>,
        in_flight: FuturesUnordered<MaybeSendBoxFuture<(ChunkAddress, SwarmResult<VerifiedChunk>)>>,
        limit: usize,
    }
}

/// Retrieve one chunk and prove its address equals the requested one, turning
/// the delivery into a [`VerifiedChunk`]. A mismatch is an invalid-data error.
pub async fn retrieve_verified<P>(provider: P, address: ChunkAddress) -> SwarmResult<VerifiedChunk>
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
        served_by: result.served_by,
    })
}

/// Admit pending addresses until the in-flight set hits the limit or the queue
/// drains. Each future carries its address out so a completion-ordered item
/// stays correlatable.
fn get_refill<P>(
    provider: &P,
    pending: &mut VecDeque<ChunkAddress>,
    in_flight: &mut FuturesUnordered<
        MaybeSendBoxFuture<(ChunkAddress, SwarmResult<VerifiedChunk>)>,
    >,
    limit: usize,
) where
    P: SwarmChunkProvider + Clone + 'static,
{
    while in_flight.len() < limit {
        let Some(address) = pending.pop_front() else {
            break;
        };
        let provider = provider.clone();
        in_flight.push(Box::pin(async move {
            (address, retrieve_verified(provider, address).await)
        }));
    }
}

impl<P> Stream for GetStream<P>
where
    P: SwarmChunkProvider + Clone + 'static,
{
    type Item = (ChunkAddress, SwarmResult<VerifiedChunk>);

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();

        // Top the window up before polling it, so a drained slot is refilled.
        get_refill(this.provider, this.pending, this.in_flight, *this.limit);

        match this.in_flight.poll_next_unpin(cx) {
            // Backpressure: refill admits exactly one replacement per drained
            // slot, so a consumer that stops polling stops all new requests.
            Poll::Ready(Some(item)) => {
                get_refill(this.provider, this.pending, this.in_flight, *this.limit);
                Poll::Ready(Some(item))
            }
            Poll::Ready(None) if this.pending.is_empty() => Poll::Ready(None),
            // Empty in-flight set but addresses remain; refill admitted them
            // above, so re-poll on the next wake.
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

/// Capability alias for a lean chunk client: retrieves, sends, cloneable, and
/// shareable across threads. Blanket-impl, so callers never name it.
///
/// `Send + Sync + 'static` holds even on wasm (a client is shared behind an
/// `Arc<dyn SwarmChunkProvider>`); the per-call `Send` divergence lives in
/// [`MaybeSend`] on the [`ChunkClientExt`] return types instead.
pub trait ChunkClient:
    SwarmChunkProvider + SwarmChunkSender + Clone + Send + Sync + 'static
{
}

impl<T> ChunkClient for T where
    T: SwarmChunkProvider + SwarmChunkSender + Clone + Send + Sync + 'static
{
}

/// Ergonomic chunk verbs (`get` / `put` / `get_many` / `put_many`) over a
/// [`ChunkClient`], blanket-implemented as default methods. The single-chunk
/// methods return `impl Future + MaybeSend` (RPITIT) so the future stays `Send`
/// on native; the bulk methods return [`GetStream`] / [`PutStream`].
pub trait ChunkClientExt: ChunkClient {
    /// Retrieve and verify one chunk via [`retrieve_verified`].
    fn get(
        &self,
        address: ChunkAddress,
    ) -> impl std::future::Future<Output = SwarmResult<VerifiedChunk>> + MaybeSend {
        retrieve_verified(self.clone(), address)
    }

    /// Upload one stamped chunk. `validate` selects [`SwarmChunkSender::send_chunk`]
    /// (checks the stamp) over [`SwarmChunkSender::send_chunk_unchecked`].
    fn put(
        &self,
        chunk: StampedChunk,
        validate: bool,
    ) -> impl std::future::Future<Output = SwarmResult<PushReceipt>> + MaybeSend {
        let client = self.clone();
        async move {
            if validate {
                client.send_chunk(chunk).await
            } else {
                client.send_chunk_unchecked(chunk).await
            }
        }
    }

    /// Retrieve and verify many chunks; see [`get_stream`].
    fn get_many(
        &self,
        addresses: impl IntoIterator<Item = ChunkAddress>,
        config: StreamConfig,
    ) -> GetStream<Self> {
        get_stream(self.clone(), addresses, config)
    }

    /// Upload many stamped chunks; see [`put_stream`].
    fn put_many<I>(&self, chunks: I, config: StreamConfig) -> PutStream<Self>
    where
        I: IntoIterator<Item = StampedChunk>,
        I::IntoIter: MaybeSendIter + 'static,
    {
        put_stream(self.clone(), chunks, config)
    }
}

impl<T: ChunkClient> ChunkClientExt for T {}

/// Like [`get_stream`], but sourced from a [`Stream`] of addresses (a wire
/// stream) instead of an eager iterator. Same bounded-prefetch and
/// completion-order semantics: a new address is pulled only as a slot frees.
pub fn get_stream_from<P, St>(
    provider: P,
    addresses: St,
    config: StreamConfig,
) -> GetStreamFrom<P, St>
where
    P: SwarmChunkProvider + Clone + 'static,
    St: Stream<Item = ChunkAddress> + MaybeSendStream,
{
    GetStreamFrom {
        provider,
        source: Some(addresses),
        in_flight: FuturesUnordered::new(),
        limit: config.max_concurrency.max(1),
    }
}

pin_project_lite::pin_project! {
    /// Stream returned by [`get_stream_from`]. Each poll pulls ready addresses
    /// from the source up to the limit, then yields the next completed
    /// retrieval; the source is taken to `None` once exhausted.
    #[must_use = "a stream does nothing unless polled"]
    pub struct GetStreamFrom<P, St> {
        provider: P,
        #[pin]
        source: Option<St>,
        in_flight: FuturesUnordered<MaybeSendBoxFuture<(ChunkAddress, SwarmResult<VerifiedChunk>)>>,
        limit: usize,
    }
}

impl<P, St> Stream for GetStreamFrom<P, St>
where
    P: SwarmChunkProvider + Clone + 'static,
    St: Stream<Item = ChunkAddress>,
{
    type Item = (ChunkAddress, SwarmResult<VerifiedChunk>);

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        // Admit ready addresses up to the limit. A pending source stops admitting
        // without parking the pipeline, since in-flight work may still complete.
        let mut source_pending = false;
        while this.in_flight.len() < *this.limit {
            let Some(source) = this.source.as_mut().as_pin_mut() else {
                break;
            };
            match source.poll_next(cx) {
                Poll::Ready(Some(address)) => {
                    let provider = this.provider.clone();
                    this.in_flight.push(Box::pin(async move {
                        (address, retrieve_verified(provider, address).await)
                    }));
                }
                // Drained: drop the source so it is never polled again.
                Poll::Ready(None) => {
                    this.source.set(None);
                    break;
                }
                Poll::Pending => {
                    source_pending = true;
                    break;
                }
            }
        }

        match this.in_flight.poll_next_unpin(cx) {
            Poll::Ready(Some(item)) => Poll::Ready(Some(item)),
            Poll::Ready(None) if this.source.is_none() => Poll::Ready(None),
            // Nothing in flight but the source could still yield. If it parked it
            // will wake us; otherwise an address was admitted this tick, re-poll.
            Poll::Ready(None) => {
                if !source_pending {
                    cx.waker().wake_by_ref();
                }
                Poll::Pending
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        // Lazy source: only the in-flight set is a known lower bound.
        (self.in_flight.len(), None)
    }
}

/// Stream of push receipts for a list of chunks to upload.
///
/// Yields `(address, result)` per input chunk in completion order, at most
/// [`StreamConfig::max_concurrency`] pushes at once. `chunks` is pulled lazily as
/// pushes are admitted, so a caller that builds chunks on demand holds at most
/// `max_concurrency` of them resident, not the whole list.
pub fn put_stream<S, I>(sender: S, chunks: I, config: StreamConfig) -> PutStream<S>
where
    S: SwarmChunkSender + Clone + 'static,
    I: IntoIterator<Item = StampedChunk>,
    I::IntoIter: MaybeSendIter + 'static,
{
    try_put_stream(
        sender,
        chunks.into_iter().map(|c| (*c.address(), Ok(c))),
        config,
    )
}

/// Stream of push receipts for a feed of fallibly-produced chunks.
///
/// Like [`put_stream`], but each entry pairs an `address` with a
/// `SwarmResult<StampedChunk>`: a per-chunk build failure surfaces as that
/// address's error item (issuing no push) instead of aborting the upload.
pub fn try_put_stream<S, I>(sender: S, chunks: I, config: StreamConfig) -> PutStream<S>
where
    S: SwarmChunkSender + Clone + 'static,
    I: IntoIterator<Item = (ChunkAddress, SwarmResult<StampedChunk>)>,
    I::IntoIter: MaybeSendIter + 'static,
{
    PutStream {
        sender,
        pending: BoxedChunks::box_chunks(chunks.into_iter()).peekable(),
        in_flight: FuturesUnordered::new(),
        // Clamp to >= 1 so a raw `StreamConfig { max_concurrency: 0 }` literal
        // never busy-loops without admitting work.
        limit: config.max_concurrency.max(1),
    }
}

/// Boxed pending-chunk feed, `Send` on native so [`PutStream`] stays `Send`.
#[cfg(not(target_arch = "wasm32"))]
type BoxedChunks = Box<dyn Iterator<Item = (ChunkAddress, SwarmResult<StampedChunk>)> + Send>;
#[cfg(target_arch = "wasm32")]
type BoxedChunks = Box<dyn Iterator<Item = (ChunkAddress, SwarmResult<StampedChunk>)>>;

/// Box a feed iterator into [`BoxedChunks`] with the per-target `Send`-ness.
trait BoxedChunksExt {
    fn box_chunks<
        I: Iterator<Item = (ChunkAddress, SwarmResult<StampedChunk>)> + MaybeSendIter + 'static,
    >(
        iter: I,
    ) -> Self;
}

impl BoxedChunksExt for BoxedChunks {
    fn box_chunks<
        I: Iterator<Item = (ChunkAddress, SwarmResult<StampedChunk>)> + MaybeSendIter + 'static,
    >(
        iter: I,
    ) -> Self {
        Box::new(iter)
    }
}

/// Source the upload pipeline pulls chunks from. Boxed so the stream type is
/// independent of how the caller produces chunks; peekable so the stream can
/// detect the end.
type PendingChunks = std::iter::Peekable<BoxedChunks>;

pin_project_lite::pin_project! {
    /// Stream returned by [`put_stream`]. Admits up to `limit` pushes at once,
    /// pulling pending chunks lazily so only admitted chunks are materialized.
    #[must_use = "a stream does nothing unless polled"]
    pub struct PutStream<S> {
        sender: S,
        pending: PendingChunks,
        in_flight: FuturesUnordered<MaybeSendBoxFuture<(ChunkAddress, SwarmResult<PushReceipt>)>>,
        limit: usize,
    }
}

/// Push one chunk, carrying its address out alongside the receipt for
/// correlation.
async fn push_chunk<S>(
    sender: S,
    address: ChunkAddress,
    chunk: StampedChunk,
) -> (ChunkAddress, SwarmResult<PushReceipt>)
where
    S: SwarmChunkSender,
{
    let receipt = sender.send_chunk(chunk).await;
    (address, receipt)
}

/// Admit pending chunks up to the cap. A feed error issues no push and is
/// yielded from a ready future carrying its address.
fn put_refill<S>(
    sender: &S,
    pending: &mut PendingChunks,
    in_flight: &mut FuturesUnordered<MaybeSendBoxFuture<(ChunkAddress, SwarmResult<PushReceipt>)>>,
    limit: usize,
) where
    S: SwarmChunkSender + Clone + 'static,
{
    while in_flight.len() < limit {
        match pending.next() {
            None => break,
            Some((address, Err(error))) => {
                in_flight.push(Box::pin(async move { (address, Err(error)) }));
            }
            Some((address, Ok(chunk))) => {
                let sender = sender.clone();
                in_flight.push(Box::pin(push_chunk(sender, address, chunk)));
            }
        }
    }
}

impl<S> Stream for PutStream<S>
where
    S: SwarmChunkSender + Clone + 'static,
{
    type Item = (ChunkAddress, SwarmResult<PushReceipt>);

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();

        put_refill(this.sender, this.pending, this.in_flight, *this.limit);

        match this.in_flight.poll_next_unpin(cx) {
            // Backpressure: exactly one replacement per drained slot, so a
            // consumer that stops polling stops all new pushes.
            Poll::Ready(Some((address, result))) => {
                put_refill(this.sender, this.pending, this.in_flight, *this.limit);
                Poll::Ready(Some((address, result)))
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
        // Lazy pending iterator: only its declared bound is known; in-flight is
        // exact.
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
        ChunkRetrievalResult, Stamp, StorageRadius, SwarmChunkProvider, SwarmChunkSender,
    };

    use super::*;

    fn test_stamp() -> Stamp {
        let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
        Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, sig)
    }

    /// A content chunk seeded so each test chunk has a unique address.
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
    async fn get_stream_yields_each_requested_address_once() {
        let chunks: Vec<_> = (0..8).map(chunk_for).collect();
        let addresses: Vec<_> = chunks.iter().map(|c| *c.address()).collect();
        let provider = MapProvider::new(chunks);

        let stream = get_stream(provider, addresses.clone(), StreamConfig::new(4));
        let results: Vec<_> = stream.collect().await;

        assert_eq!(results.len(), addresses.len());
        let want: std::collections::HashSet<_> = addresses.iter().copied().collect();
        let mut seen = std::collections::HashSet::new();
        for (address, result) in results {
            let verified = result.expect("retrieval succeeds");
            // Unordered: assert membership not position.
            assert_eq!(*verified.address(), address);
            seen.insert(address);
        }
        assert_eq!(seen, want);
    }

    #[tokio::test]
    async fn get_stream_caps_concurrency() {
        // A cap of 3 permits exactly 3 concurrent retrievals.
        let chunks: Vec<_> = (0..16).map(chunk_for).collect();
        let addresses: Vec<_> = chunks.iter().map(|c| *c.address()).collect();
        let provider = MapProvider::gated(chunks);
        let peak = Arc::clone(&provider.peak);
        let gate = Arc::clone(&provider.gate);
        let in_flight = Arc::clone(&provider.in_flight);

        let mut stream = get_stream(provider, addresses, StreamConfig::new(3));

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
        // Give any erroneous extra retrieval a chance to register.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(peak.load(Ordering::SeqCst) <= 3, "cap must bound fan-out");

        // Release everything so the driver can finish.
        gate.add_permits(1 << 20);
        let _ = driver.await.unwrap();
    }

    #[tokio::test]
    async fn get_stream_concurrency_of_one_streams_serially() {
        let chunks: Vec<_> = (0..4).map(chunk_for).collect();
        let addresses: Vec<_> = chunks.iter().map(|c| *c.address()).collect();
        let provider = MapProvider::new(chunks);

        // A zero cap clamps to one in flight: serial, but still makes progress.
        let stream = get_stream(provider, addresses.clone(), StreamConfig::new(0));
        let results: Vec<_> = stream.collect().await;
        assert_eq!(results.len(), addresses.len());
        assert!(results.into_iter().all(|(_, r)| r.is_ok()));
    }

    #[tokio::test]
    async fn get_stream_surfaces_missing_chunk_as_error_item_and_continues() {
        let present: Vec<_> = (0..3).map(chunk_for).collect();
        let mut addresses: Vec<_> = present.iter().map(|c| *c.address()).collect();
        // Insert a never-stored address in the middle.
        let missing = ChunkAddress::new([0xfe; 32]);
        addresses.insert(1, missing);
        let provider = MapProvider::new(present);

        let stream = get_stream(provider, addresses.clone(), StreamConfig::new(4));
        let results: Vec<_> = stream.collect().await;

        assert_eq!(results.len(), addresses.len());
        // Unordered: locate each outcome by its item address.
        for (address, result) in results {
            if address == missing {
                assert!(matches!(result, Err(SwarmError::ChunkNotFound { .. })));
            } else {
                assert!(result.is_ok());
            }
        }
    }

    #[tokio::test]
    async fn get_stream_rejects_wrong_chunk_for_address() {
        // The peer answers every request with a fixed chunk; only the request
        // for that chunk's own address may verify, the rest must error.
        let served = chunk_for(0);
        let served_address = *served.address();
        let other = *chunk_for(99).address();
        let provider = WrongChunkProvider { chunk: served };

        let stream = get_stream(provider, vec![served_address, other], StreamConfig::new(4));
        let results: Vec<_> = stream.collect().await;

        for (address, result) in results {
            if address == served_address {
                assert!(result.is_ok(), "address that matches the bytes verifies");
            } else {
                assert!(
                    matches!(result, Err(SwarmError::InvalidChunk { .. })),
                    "wrong bytes for an address must be rejected: {:?}",
                    result
                );
            }
        }
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
    /// a `VerifiedChunk` carrying no stamp.
    #[tokio::test]
    async fn get_stream_accepts_stampless_delivery() {
        let chunk = chunk_for(5).into_parts().0;
        let address = *chunk.address();
        let provider = StamplessProvider { chunk };

        let stream = get_stream(provider, vec![address], StreamConfig::new(4));
        let mut results: Vec<_> = stream.collect().await;
        let (item_address, result) = results.remove(0);
        let verified = result.expect("a stampless delivery verifies");
        assert_eq!(item_address, address);
        assert_eq!(*verified.address(), address);
        assert!(verified.stamp().is_none(), "no stamp is carried through");
    }

    /// The verified chunk carries the overlay the provider reported as `served_by`.
    #[tokio::test]
    async fn get_stream_carries_served_by() {
        let chunk = chunk_for(7);
        let address = *chunk.address();
        let provider = MapProvider::new(vec![chunk]);

        let stream = get_stream(provider, vec![address], StreamConfig::new(4));
        let mut results: Vec<_> = stream.collect().await;
        let (_, result) = results.remove(0);
        let verified = result.expect("retrieval succeeds");
        // `MapProvider` serves from this fixed overlay.
        assert_eq!(verified.served_by(), OverlayAddress::from([1u8; 32]));
    }

    #[tokio::test]
    async fn put_stream_uploads_all_chunks() {
        let chunks: Vec<_> = (0..8).map(chunk_for).collect();
        let addresses: Vec<_> = chunks.iter().map(|c| *c.address()).collect();
        let sender = RecordingSender::new();
        let accepted = Arc::clone(&sender.accepted);

        let stream = put_stream(sender, chunks, StreamConfig::new(4));
        let results: Vec<_> = stream.collect().await;

        assert_eq!(results.len(), addresses.len());
        assert!(results.into_iter().all(|(_, r)| r.is_ok()));
        // Every chunk was accepted exactly once.
        let accepted = accepted.lock().unwrap();
        assert_eq!(accepted.len(), addresses.len());
        for address in &addresses {
            assert!(accepted.contains(address));
        }
    }

    #[tokio::test]
    async fn put_stream_caps_in_flight_at_the_concurrency() {
        // A cap of 3 permits exactly 3 concurrent pushes.
        let chunks: Vec<_> = (0..16).map(chunk_for).collect();
        let sender = RecordingSender::gated();
        let peak = Arc::clone(&sender.peak);
        let gate = Arc::clone(&sender.gate);
        let in_flight = Arc::clone(&sender.in_flight);

        let mut stream = put_stream(sender, chunks, StreamConfig::new(3));
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
        assert!(peak.load(Ordering::SeqCst) <= 3, "cap must bound pushes");

        gate.add_permits(1 << 20);
        let _ = driver.await.unwrap();
    }

    #[tokio::test]
    async fn put_stream_materializes_chunks_lazily() {
        // The upload pipeline pulls from its source iterator only as it admits
        // pushes, so a caller that reconstructs each chunk on demand keeps at
        // most `max_concurrency` chunks resident, not the whole list.
        let materialized = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&materialized);
        let source = (0..1024).map(move |i| {
            counter.fetch_add(1, Ordering::SeqCst);
            chunk_for((i % 251) as u8)
        });

        let sender = RecordingSender::gated();
        let gate = Arc::clone(&sender.gate);
        let in_flight = Arc::clone(&sender.in_flight);

        let mut stream = put_stream(sender, source, StreamConfig::new(3));
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
        // Only the admitted (in-flight) chunks are pulled from the source. Far
        // below the 1024 in the list: the heap holds the cap, not the input.
        assert!(
            materialized.load(Ordering::SeqCst) <= 3,
            "lazy: at most max_concurrency chunks materialized, got {}",
            materialized.load(Ordering::SeqCst)
        );

        gate.add_permits(1 << 20);
        let _ = driver.await.unwrap();
    }

    #[tokio::test]
    async fn put_stream_concurrency_of_one_streams_serially() {
        // A zero cap clamps to one in flight: serial, but still makes progress.
        let chunks: Vec<_> = (0..4).map(chunk_for).collect();
        let sender = RecordingSender::new();
        let accepted = Arc::clone(&sender.accepted);

        let stream = put_stream(sender, chunks, StreamConfig::new(0));
        let results: Vec<_> = stream.collect().await;

        assert_eq!(results.len(), 4);
        assert!(results.into_iter().all(|(_, r)| r.is_ok()));
        assert_eq!(accepted.lock().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn put_stream_surfaces_failed_push_and_continues() {
        let chunks: Vec<_> = (0..3).map(chunk_for).collect();
        let failed = *chunks[1].address();
        let mut sender = RecordingSender::new();
        sender.fail_on = Some(failed);
        let accepted = Arc::clone(&sender.accepted);

        let stream = put_stream(sender, chunks, StreamConfig::new(4));
        let results: Vec<_> = stream.collect().await;

        assert_eq!(results.len(), 3);
        for (address, result) in results {
            if address == failed {
                assert!(matches!(result, Err(SwarmError::NoStorer { .. })));
            } else {
                assert!(result.is_ok());
            }
        }
        // The failed chunk was never recorded as accepted.
        assert_eq!(accepted.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn try_put_stream_surfaces_feed_error_and_continues() {
        // A fallible feed: one item fails to build. It must surface as an error
        // for its address, without consuming a push, and the well-formed chunks
        // must still upload.
        let good0 = chunk_for(0);
        let good2 = chunk_for(2);
        let addr0 = *good0.address();
        let addr2 = *good2.address();
        let err_addr = ChunkAddress::new([0xee; 32]);
        let sender = RecordingSender::new();
        let accepted = Arc::clone(&sender.accepted);

        let feed = vec![
            (addr0, Ok(good0)),
            (
                err_addr,
                Err(SwarmError::InvalidChunk {
                    address: None,
                    reason: "bad bytes".to_string(),
                }),
            ),
            (addr2, Ok(good2)),
        ];
        let stream = try_put_stream(sender, feed, StreamConfig::new(4));
        let results: Vec<_> = stream.collect().await;

        assert_eq!(results.len(), 3);
        for (address, result) in &results {
            if *address == err_addr {
                assert!(matches!(result, Err(SwarmError::InvalidChunk { .. })));
            } else {
                assert!(result.is_ok());
            }
        }
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
        let cfg = StreamConfig::new(0);
        assert_eq!(cfg.max_concurrency, 1);
    }

    #[test]
    fn native_download_concurrency_stays_in_sane_band() {
        // The derived value must track the bandwidth-delay product, not drift to
        // something that floods peers or starves the pipe.
        assert!(
            (64..=160).contains(&NATIVE_DOWNLOAD_CONCURRENCY),
            "derived native-download concurrency drifted: {NATIVE_DOWNLOAD_CONCURRENCY}"
        );
        assert_eq!(
            StreamConfig::NATIVE_DOWNLOAD.max_concurrency,
            NATIVE_DOWNLOAD_CONCURRENCY
        );
    }

    #[tokio::test]
    async fn get_stream_from_yields_each_requested_address_once() {
        let chunks: Vec<_> = (0..8).map(chunk_for).collect();
        let addresses: Vec<_> = chunks.iter().map(|c| *c.address()).collect();
        let provider = MapProvider::new(chunks);

        let source = futures::stream::iter(addresses.clone());
        let stream = get_stream_from(provider, source, StreamConfig::new(4));
        let results: Vec<_> = stream.collect().await;

        assert_eq!(results.len(), addresses.len());
        let want: std::collections::HashSet<_> = addresses.iter().copied().collect();
        let mut seen = std::collections::HashSet::new();
        for (address, result) in results {
            let verified = result.expect("retrieval succeeds");
            // Order is the consumer's job; assert membership not position.
            assert_eq!(*verified.address(), address);
            seen.insert(address);
        }
        assert_eq!(seen, want);
    }

    #[tokio::test]
    async fn get_stream_from_caps_concurrency() {
        // A cap of 3 permits exactly 3 concurrent retrievals even with many
        // addresses queued in the source.
        let chunks: Vec<_> = (0..16).map(chunk_for).collect();
        let addresses: Vec<_> = chunks.iter().map(|c| *c.address()).collect();
        let provider = MapProvider::gated(chunks);
        let peak = Arc::clone(&provider.peak);
        let gate = Arc::clone(&provider.gate);
        let in_flight = Arc::clone(&provider.in_flight);

        let source = futures::stream::iter(addresses);
        let mut stream = get_stream_from(provider, source, StreamConfig::new(3));

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
        assert!(peak.load(Ordering::SeqCst) <= 3, "cap must bound fan-out");

        gate.add_permits(1 << 20);
        let _ = driver.await.unwrap();
    }

    #[tokio::test]
    async fn get_stream_from_concurrency_of_one_streams_serially() {
        let chunks: Vec<_> = (0..4).map(chunk_for).collect();
        let addresses: Vec<_> = chunks.iter().map(|c| *c.address()).collect();
        let provider = MapProvider::new(chunks);

        let source = futures::stream::iter(addresses.clone());
        let stream = get_stream_from(provider, source, StreamConfig::new(0));
        let results: Vec<_> = stream.collect().await;
        assert_eq!(results.len(), addresses.len());
        assert!(results.into_iter().all(|(_, r)| r.is_ok()));
    }

    #[tokio::test]
    async fn get_stream_from_surfaces_missing_chunk_as_error_item_and_continues() {
        let present: Vec<_> = (0..3).map(chunk_for).collect();
        let mut addresses: Vec<_> = present.iter().map(|c| *c.address()).collect();
        let missing = ChunkAddress::new([0xfe; 32]);
        addresses.insert(1, missing);
        let provider = MapProvider::new(present);

        let source = futures::stream::iter(addresses.clone());
        let stream = get_stream_from(provider, source, StreamConfig::new(4));
        let results: Vec<_> = stream.collect().await;

        assert_eq!(results.len(), addresses.len());
        for (address, result) in results {
            if address == missing {
                assert!(matches!(result, Err(SwarmError::ChunkNotFound { .. })));
            } else {
                assert!(result.is_ok());
            }
        }
    }

    #[tokio::test]
    async fn get_stream_from_rejects_wrong_chunk_for_address() {
        let served = chunk_for(0);
        let served_address = *served.address();
        let other = *chunk_for(99).address();
        let provider = WrongChunkProvider { chunk: served };

        let source = futures::stream::iter(vec![served_address, other]);
        let stream = get_stream_from(provider, source, StreamConfig::new(4));
        let results: Vec<_> = stream.collect().await;

        for (address, result) in results {
            if address == served_address {
                assert!(result.is_ok(), "address that matches the bytes verifies");
            } else {
                assert!(
                    matches!(result, Err(SwarmError::InvalidChunk { .. })),
                    "wrong bytes for an address must be rejected: {:?}",
                    result
                );
            }
        }
    }

    #[tokio::test]
    async fn get_stream_from_carries_served_by() {
        let chunk = chunk_for(7);
        let address = *chunk.address();
        let provider = MapProvider::new(vec![chunk]);

        let source = futures::stream::iter(vec![address]);
        let stream = get_stream_from(provider, source, StreamConfig::new(4));
        let mut results: Vec<_> = stream.collect().await;
        let (_, result) = results.remove(0);
        let verified = result.expect("retrieval succeeds");
        assert_eq!(verified.served_by(), OverlayAddress::from([1u8; 32]));
    }

    #[tokio::test]
    async fn get_stream_from_empty_source_terminates() {
        let provider = MapProvider::new(vec![]);
        let source = futures::stream::iter(Vec::new());
        let stream = get_stream_from(provider, source, StreamConfig::DEFAULT);
        let results: Vec<_> = stream.collect().await;
        assert!(results.is_empty());
    }

    #[test]
    fn parse_address_accepts_32_bytes_and_rejects_others() {
        let bytes = [0xab; 32];
        let address = parse_address(&bytes).expect("32 bytes parse");
        assert_eq!(address.as_bytes(), &bytes);

        let err = parse_address(&[0u8; 10]).expect_err("short address fails");
        assert_eq!(err.got, 10);
    }
    /// A client mock satisfying [`ChunkClient`]: it both serves a fixed chunk map
    /// and records pushes, so the `ChunkClientExt` verbs can be driven end to end.
    #[derive(Clone)]
    struct ClientMock {
        provider: MapProvider,
        sender: RecordingSender,
    }

    impl ClientMock {
        fn new(chunks: Vec<StampedChunk>) -> Self {
            Self {
                provider: MapProvider::new(chunks),
                sender: RecordingSender::new(),
            }
        }
    }

    #[async_trait]
    impl SwarmChunkProvider for ClientMock {
        async fn retrieve_chunk(
            &self,
            address: &ChunkAddress,
        ) -> SwarmResult<ChunkRetrievalResult> {
            self.provider.retrieve_chunk(address).await
        }

        fn has_chunk(&self, address: &ChunkAddress) -> bool {
            self.provider.has_chunk(address)
        }
    }

    #[async_trait]
    impl SwarmChunkSender for ClientMock {
        async fn send_chunk_unchecked(&self, chunk: StampedChunk) -> SwarmResult<PushReceipt> {
            self.sender.send_chunk_unchecked(chunk).await
        }

        async fn send_chunk(&self, chunk: StampedChunk) -> SwarmResult<PushReceipt> {
            self.sender.send_chunk(chunk).await
        }
    }

    #[tokio::test]
    async fn ext_get_verifies_and_returns_the_chunk() {
        let chunk = chunk_for(11);
        let address = *chunk.address();
        let client = ClientMock::new(vec![chunk]);

        let verified = client.get(address).await.expect("retrieval succeeds");
        assert_eq!(*verified.address(), address);
    }

    #[tokio::test]
    async fn ext_get_rejects_wrong_chunk_for_address() {
        // A request for a never-stored address surfaces as an error, not a chunk.
        let client = ClientMock::new(vec![chunk_for(11)]);
        let missing = ChunkAddress::new([0xfe; 32]);
        let err = client.get(missing).await.expect_err("missing chunk errors");
        assert!(matches!(err, SwarmError::ChunkNotFound { .. }));
    }

    #[tokio::test]
    async fn ext_put_validated_and_unchecked_both_upload() {
        let validated = chunk_for(20);
        let unchecked = chunk_for(21);
        let addr_validated = *validated.address();
        let addr_unchecked = *unchecked.address();
        let client = ClientMock::new(vec![]);
        let accepted = Arc::clone(&client.sender.accepted);

        client.put(validated, true).await.expect("validated push");
        client.put(unchecked, false).await.expect("unchecked push");

        let accepted = accepted.lock().unwrap();
        assert!(accepted.contains(&addr_validated));
        assert!(accepted.contains(&addr_unchecked));
    }

    #[tokio::test]
    async fn ext_get_many_streams_all_addresses() {
        let chunks: Vec<_> = (0..6).map(chunk_for).collect();
        let addresses: Vec<_> = chunks.iter().map(|c| *c.address()).collect();
        let client = ClientMock::new(chunks);

        let stream = client.get_many(addresses.clone(), StreamConfig::new(4));
        let results: Vec<_> = stream.collect().await;

        assert_eq!(results.len(), addresses.len());
        let want: std::collections::HashSet<_> = addresses.into_iter().collect();
        let seen: std::collections::HashSet<_> = results
            .into_iter()
            .map(|(address, result)| {
                result.expect("retrieval succeeds");
                address
            })
            .collect();
        assert_eq!(seen, want);
    }

    #[tokio::test]
    async fn ext_put_many_uploads_all_chunks() {
        let chunks: Vec<_> = (0..6).map(chunk_for).collect();
        let addresses: Vec<_> = chunks.iter().map(|c| *c.address()).collect();
        let client = ClientMock::new(vec![]);
        let accepted = Arc::clone(&client.sender.accepted);

        let stream = client.put_many(chunks, StreamConfig::new(4));
        let results: Vec<_> = stream.collect().await;

        assert_eq!(results.len(), addresses.len());
        assert!(results.into_iter().all(|(_, r)| r.is_ok()));
        let accepted = accepted.lock().unwrap();
        for address in &addresses {
            assert!(accepted.contains(address));
        }
    }

    /// The native `get`/`put` futures must be `Send` so the generic tonic handler
    /// can hold them: drive each through `tokio::spawn`, which requires `Send`.
    /// This is the compile-and-run guard for the RPITIT `+ MaybeSend` bound.
    #[tokio::test]
    async fn ext_futures_are_send_and_spawnable() {
        let chunk = chunk_for(30);
        let address = *chunk.address();
        let client = ClientMock::new(vec![chunk_for(30)]);

        let get_client = client.clone();
        let verified = tokio::spawn(async move { get_client.get(address).await })
            .await
            .expect("get task joins")
            .expect("retrieval succeeds");
        assert_eq!(*verified.address(), address);

        let put_client = client.clone();
        let receipt = tokio::spawn(async move { put_client.put(chunk, true).await })
            .await
            .expect("put task joins");
        assert!(receipt.is_ok());
    }
}
