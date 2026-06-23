//! Wasm-buildable proximity-routing chunk provider/sender for the browser client.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use nectar_primitives::SwarmAddress;
use vertex_swarm_api::{
    ChunkAddress, ChunkRetrievalResult, PushReceipt, StampedChunk, SwarmChunkProvider,
    SwarmChunkSender, SwarmError, SwarmResult, SwarmTopologyRouting,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_node::{ChunkTransferError, ClientHandle, RaceFailure, race_candidates};
use vertex_swarm_primitives::OverlayAddress;
use vertex_swarm_topology::TopologyHandle;

/// Closest peers to try when pushing a chunk before giving up.
const PUSH_CANDIDATE_COUNT: usize = 5;

/// Connected peers raced together in the first retrieval wave.
///
/// The first wave is a latency hedge: more parallel legs find a fast responder
/// sooner, which lowers per-chunk latency and (at fixed aggregate concurrency)
/// raises throughput. It also multiplies single-thread substream work, so the
/// width trades thread cost against tail latency. Tunable at runtime via the
/// `rw` URL param to sweep without rebuilding.
const DEFAULT_RETRIEVE_INITIAL_CANDIDATES: usize = 8;

/// Additional connected peers admitted to the race on each later fallback wave.
const DEFAULT_RETRIEVE_WAVE_STEP: usize = 8;

/// Stagger between retrieval candidates joining a wave's race. Tunable via the
/// `stagger` URL param (milliseconds).
const DEFAULT_RETRIEVAL_STAGGER_MS: u64 = 500;

static RETRIEVE_INITIAL_CANDIDATES: AtomicU64 =
    AtomicU64::new(DEFAULT_RETRIEVE_INITIAL_CANDIDATES as u64);
static RETRIEVE_WAVE_STEP: AtomicU64 = AtomicU64::new(DEFAULT_RETRIEVE_WAVE_STEP as u64);
static RETRIEVAL_STAGGER_MS: AtomicU64 = AtomicU64::new(DEFAULT_RETRIEVAL_STAGGER_MS);

static RETRIEVE_CLOSE_BUDGET_A: AtomicU64 = AtomicU64::new(DEFAULT_RETRIEVE_CLOSE_BUDGET as u64);
static RETRIEVE_BUSY_RETRIES_A: AtomicU64 = AtomicU64::new(DEFAULT_RETRIEVE_BUSY_RETRIES as u64);

/// Override the retrieval race knobs from the page URL (`rw`, `wavestep`,
/// `stagger`, `budget`, `busy`). A measurement aid so a sweep needs no rebuild;
/// absent params keep the defaults.
#[allow(clippy::too_many_arguments)]
pub fn configure_retrieval_race(
    initial: Option<u64>,
    wave_step: Option<u64>,
    stagger_ms: Option<u64>,
    budget: Option<u64>,
    busy: Option<u64>,
) {
    if let Some(v) = initial.filter(|v| *v > 0) {
        RETRIEVE_INITIAL_CANDIDATES.store(v, Ordering::Relaxed);
    }
    if let Some(v) = wave_step.filter(|v| *v > 0) {
        RETRIEVE_WAVE_STEP.store(v, Ordering::Relaxed);
    }
    if let Some(v) = stagger_ms.filter(|v| *v > 0) {
        RETRIEVAL_STAGGER_MS.store(v, Ordering::Relaxed);
    }
    if let Some(v) = budget.filter(|v| *v > 0) {
        RETRIEVE_CLOSE_BUDGET_A.store(v, Ordering::Relaxed);
    }
    // `busy` may legitimately be set to 0 to disable re-racing.
    if let Some(v) = busy {
        RETRIEVE_BUSY_RETRIES_A.store(v, Ordering::Relaxed);
    }
}

fn retrieval_stagger() -> Duration {
    Duration::from_millis(RETRIEVAL_STAGGER_MS.load(Ordering::Relaxed))
}

fn retrieve_close_budget() -> usize {
    RETRIEVE_CLOSE_BUDGET_A.load(Ordering::Relaxed) as usize
}

fn retrieve_busy_retries() -> usize {
    RETRIEVE_BUSY_RETRIES_A.load(Ordering::Relaxed) as usize
}

/// Single-thread instrumentation: retrieval legs dispatched and chunks served.
///
/// A "leg" is one `client.retrieve_chunk` substream attempt. The dispatched/hit
/// ratio is substreams-per-chunk, the headline multiplier on the single wasm
/// thread. Reported periodically from the hit path; temporary measurement aid.
static LEGS_DISPATCHED: AtomicU64 = AtomicU64::new(0);
static CHUNKS_SERVED: AtomicU64 = AtomicU64::new(0);
/// Per-leg outcome tally, to separate a peer-coverage miss (the remote forwarded
/// but no closer peer held the chunk: `Remote`/`NotFound`) from a transport
/// failure (`Protocol`/`NotConnected`/`Cancelled`/`ChannelClosed`). Temporary
/// measurement aid surfaced in the periodic instrumentation line.
static LEG_REMOTE: AtomicU64 = AtomicU64::new(0);
static LEG_NOTFOUND: AtomicU64 = AtomicU64::new(0);
static LEG_TIMEOUT: AtomicU64 = AtomicU64::new(0);
static LEG_PROTOCOL: AtomicU64 = AtomicU64::new(0);
static LEG_NOTCONN: AtomicU64 = AtomicU64::new(0);
static LEG_CANCELLED: AtomicU64 = AtomicU64::new(0);
static LEG_CHANCLOSED: AtomicU64 = AtomicU64::new(0);
static LEG_BUSY: AtomicU64 = AtomicU64::new(0);
/// Total `closest_to` wall time (microseconds) and call count, to size the
/// per-chunk proximity ranking against the rest of the per-chunk thread work.
static CLOSEST_TO_US: AtomicU64 = AtomicU64::new(0);
static CLOSEST_TO_CALLS: AtomicU64 = AtomicU64::new(0);

/// Hard ceiling on connected peers a retrieval races before it stops widening.
///
/// A retrieval never dials: it relies on the serving peer to forward the request
/// toward the chunk's neighbourhood (the retrieval protocol is forwarding-based).
/// Racing the closest connected peers is enough, so this is the only budget.
/// Tunable via the `budget` URL param.
const DEFAULT_RETRIEVE_CLOSE_BUDGET: usize = 24;

/// Times a retrieval re-races its candidates when every one was skipped for
/// being at its per-peer in-flight cap.
///
/// An all-busy wave is transient self-imposed back-pressure (a wide parallel
/// download momentarily filled every close peer's slots), not chunk absence, so
/// the retrieval waits a short backoff and re-races rather than failing the
/// chunk. Tunable via the `busy` URL param.
const DEFAULT_RETRIEVE_BUSY_RETRIES: usize = 12;

/// Backoff between all-busy re-races, long enough for an in-flight slot to free.
const RETRIEVE_BUSY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(50);

/// A proximity-routing chunk provider/sender over the browser client node.
#[derive(Clone)]
pub struct BrowserChunkProvider {
    client: ClientHandle,
    topology: TopologyHandle<Arc<Identity>>,
}

impl BrowserChunkProvider {
    /// Build the provider from the launched client's handle and topology.
    pub fn new(client: ClientHandle, topology: TopologyHandle<Arc<Identity>>) -> Self {
        Self { client, topology }
    }
}

#[async_trait::async_trait]
impl SwarmChunkProvider for BrowserChunkProvider {
    /// Retrieve a chunk by racing the closest connected peers in widening waves.
    ///
    /// The browser never dials on a retrieval. The retrieval protocol is
    /// forwarding-based: a connected peer that does not hold the chunk relays the
    /// request to a peer strictly closer to the chunk and returns the answer, so
    /// racing the closest connected peers reaches the chunk's neighbourhood
    /// without the client opening any new connection. A terminal absence surfaces
    /// as `NotFound`.
    async fn retrieve_chunk(&self, address: &ChunkAddress) -> SwarmResult<ChunkRetrievalResult> {
        let chunk_address = SwarmAddress::new(address.0.into());

        // Rank the connected peers by proximity to the chunk, but only the
        // closest `retrieve_close_budget()` of them.
        let closest_started = js_sys::Date::now();
        let ranked = self
            .topology
            .closest_to(&chunk_address, retrieve_close_budget());
        CLOSEST_TO_US.fetch_add(
            ((js_sys::Date::now() - closest_started) * 1000.0) as u64,
            Ordering::Relaxed,
        );
        CLOSEST_TO_CALLS.fetch_add(1, Ordering::Relaxed);

        if ranked.is_empty() {
            tracing::warn!(
                chunk = %chunk_address,
                "retrieval: no connected peers available"
            );
            return Err(SwarmError::network_msg(
                "no connected peers available for retrieval",
            ));
        }

        // Re-race on an all-busy wave: every close peer was momentarily at its
        // in-flight cap (a wide parallel download), which is transient
        // back-pressure, not absence. Wait a short backoff and re-race.
        let mut outcome = self.race_connected_waves(&ranked, chunk_address).await;
        for _ in 0..retrieve_busy_retries() {
            if !matches!(&outcome, WaveOutcome::Failed(ChunkTransferError::Busy)) {
                break;
            }
            futures_timer::Delay::new(RETRIEVE_BUSY_BACKOFF).await;
            outcome = self.race_connected_waves(&ranked, chunk_address).await;
        }

        match outcome {
            WaveOutcome::Hit(result) => {
                let served = CHUNKS_SERVED.fetch_add(1, Ordering::Relaxed) + 1;
                // Report the single-thread breakdown once per 100 chunks: the
                // substreams-per-chunk multiplier and the mean `closest_to`
                // cost, so iteration can see where the per-chunk thread time
                // goes. Temporary measurement aid.
                if served.is_multiple_of(20) {
                    let legs = LEGS_DISPATCHED.load(Ordering::Relaxed);
                    let ct_us = CLOSEST_TO_US.load(Ordering::Relaxed);
                    let ct_calls = CLOSEST_TO_CALLS.load(Ordering::Relaxed).max(1);
                    let spc = (legs as f64 / served as f64 * 100.0).round() / 100.0;
                    // Single pre-formatted message: the browser console formatter
                    // splits structured fields into separate args the harness
                    // cannot scrape, so everything goes in the message string.
                    let remote = LEG_REMOTE.load(Ordering::Relaxed);
                    let notfound = LEG_NOTFOUND.load(Ordering::Relaxed);
                    let timeout = LEG_TIMEOUT.load(Ordering::Relaxed);
                    let protocol = LEG_PROTOCOL.load(Ordering::Relaxed);
                    let notconn = LEG_NOTCONN.load(Ordering::Relaxed);
                    let cancelled = LEG_CANCELLED.load(Ordering::Relaxed);
                    let chanclosed = LEG_CHANCLOSED.load(Ordering::Relaxed);
                    let busy = LEG_BUSY.load(Ordering::Relaxed);
                    tracing::info!(
                        "retrieval-instrumentation served={served} legs={legs} \
                         substreams_per_chunk={spc} closest_to_us_mean={} closest_to_calls={ct_calls} \
                         leg_remote={remote} leg_notfound={notfound} leg_timeout={timeout} \
                         leg_protocol={protocol} leg_notconn={notconn} \
                         leg_cancelled={cancelled} leg_chanclosed={chanclosed} leg_busy={busy}",
                        ct_us / ct_calls,
                    );
                }
                tracing::debug!(
                    chunk = %chunk_address,
                    served_by = %result.served_by,
                    server_po = chunk_address.proximity(&result.served_by).get(),
                    close_peers = ranked.len(),
                    "retrieval: succeeded over connected peers (forwarding)"
                );
                Ok(result)
            }
            WaveOutcome::NotFound => {
                tracing::warn!(
                    chunk = %chunk_address,
                    close_peers = ranked.len(),
                    "retrieval: not found across connected peers (forwarded-but-absent)"
                );
                Err(SwarmError::ChunkNotFound { address: *address })
            }
            WaveOutcome::Failed(e) => {
                tracing::warn!(
                    chunk = %chunk_address,
                    close_peers = ranked.len(),
                    error = %e,
                    "retrieval: all connected peers failed"
                );
                Err(SwarmError::AllPeersFailed {
                    address: *address,
                    attempts: ranked.len(),
                    source: Box::new(e),
                })
            }
            WaveOutcome::NoCandidates => Err(SwarmError::network_msg(
                "no connected peers available for retrieval",
            )),
        }
    }

    fn has_chunk(&self, _address: &ChunkAddress) -> bool {
        // Client nodes have no local storage.
        false
    }
}

/// Outcome of the close-peer wave phase.
enum WaveOutcome {
    /// A connected peer served the chunk.
    Hit(ChunkRetrievalResult),
    /// Every attempted wave failed and the terminal failure was `NotFound`.
    NotFound,
    /// Every attempted wave failed with a retryable (non-`NotFound`) error.
    Failed(ChunkTransferError),
    /// The candidate slice was empty.
    NoCandidates,
}

impl BrowserChunkProvider {
    /// Race the proximity-ranked connected peers in widening waves to the first answer.
    ///
    /// Each attempt logs the serving peer's proximity to the chunk at debug: a
    /// connected peer serving a chunk far from itself is the observable signature
    /// of the remote forwarding the request onward.
    async fn race_connected_waves(
        &self,
        ranked: &[OverlayAddress],
        chunk_address: SwarmAddress,
    ) -> WaveOutcome {
        let available = ranked.len();
        let mut attempted = 0usize;
        let mut wave = 0usize;
        let mut last_failure: Option<ChunkTransferError> = None;

        while attempted < available {
            let wave_len = if wave == 0 {
                RETRIEVE_INITIAL_CANDIDATES.load(Ordering::Relaxed) as usize
            } else {
                RETRIEVE_WAVE_STEP.load(Ordering::Relaxed) as usize
            };
            let wave_end = (attempted + wave_len).min(available);
            let slice: Vec<_> = ranked[attempted..wave_end].to_vec();
            let wave_size = slice.len();
            if wave_size == 0 {
                break;
            }

            match race_candidates(slice, retrieval_stagger(), |peer| {
                let client = self.client.clone();
                async move {
                    LEGS_DISPATCHED.fetch_add(1, Ordering::Relaxed);
                    let peer_po = chunk_address.proximity(&peer).get();
                    let started = js_sys::Date::now();
                    let outcome = client.retrieve_chunk(peer, chunk_address, true).await;
                    let latency_ms = js_sys::Date::now() - started;
                    match &outcome {
                        Ok(_) => {}
                        Err(ChunkTransferError::Remote) => {
                            LEG_REMOTE.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(ChunkTransferError::NotFound(_)) => {
                            LEG_NOTFOUND.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(ChunkTransferError::TimedOut) => {
                            LEG_TIMEOUT.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(ChunkTransferError::Protocol(_)) => {
                            LEG_PROTOCOL.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(ChunkTransferError::NotConnected) => {
                            LEG_NOTCONN.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(ChunkTransferError::Cancelled) => {
                            LEG_CANCELLED.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(ChunkTransferError::ChannelClosed) => {
                            LEG_CHANCLOSED.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(ChunkTransferError::Busy) => {
                            LEG_BUSY.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    match &outcome {
                        Ok(_) => tracing::debug!(
                            %peer,
                            chunk = %chunk_address,
                            peer_po,
                            latency_ms,
                            "retrieval attempt: Hit"
                        ),
                        Err(ChunkTransferError::NotFound(_)) => tracing::debug!(
                            %peer,
                            chunk = %chunk_address,
                            peer_po,
                            latency_ms,
                            "retrieval attempt: NotFound"
                        ),
                        Err(ChunkTransferError::TimedOut) => tracing::debug!(
                            %peer,
                            chunk = %chunk_address,
                            peer_po,
                            latency_ms,
                            "retrieval attempt: Timeout"
                        ),
                        Err(e) => tracing::debug!(
                            %peer,
                            chunk = %chunk_address,
                            peer_po,
                            latency_ms,
                            error = %e,
                            "retrieval attempt: transport-error"
                        ),
                    }
                    outcome
                }
            })
            .await
            {
                Ok(result) => {
                    return WaveOutcome::Hit(ChunkRetrievalResult {
                        chunk: result.chunk,
                        stamp: result.stamp,
                        served_by: result.peer,
                    });
                }
                Err(RaceFailure::NoCandidates) => break,
                Err(RaceFailure::AllFailed(e)) => {
                    attempted += wave_size;
                    last_failure = Some(e);
                    wave += 1;
                }
            }
        }

        match last_failure {
            Some(ChunkTransferError::NotFound(_)) => WaveOutcome::NotFound,
            Some(e) => WaveOutcome::Failed(e),
            None => WaveOutcome::NoCandidates,
        }
    }
}

#[async_trait::async_trait]
impl SwarmChunkSender for BrowserChunkProvider {
    async fn send_chunk_unchecked(&self, chunk: StampedChunk) -> SwarmResult<PushReceipt> {
        push_to_closest(&self.client, &self.topology, chunk).await
    }

    async fn send_chunk(&self, chunk: StampedChunk) -> SwarmResult<PushReceipt> {
        let address = *chunk.address();
        chunk
            .stamp()
            .recover_signer(&address)
            .map_err(|err| SwarmError::InvalidSignature {
                chunk_address: address,
                reason: err.to_string(),
            })?;
        push_to_closest(&self.client, &self.topology, chunk).await
    }
}

/// Push `chunk` to the closest storers sequentially, returning the first receipt.
async fn push_to_closest(
    client: &ClientHandle,
    topology: &TopologyHandle<Arc<Identity>>,
    chunk: StampedChunk,
) -> SwarmResult<PushReceipt> {
    let address = *chunk.address();
    let closest = topology.closest_to(&address, PUSH_CANDIDATE_COUNT);
    let attempts = closest.len();

    let mut outcome = Err(SwarmError::NoStorer {
        chunk_address: address,
    });
    for peer in closest {
        match client.push_chunk(peer, chunk.clone(), true).await {
            Ok(receipt) => {
                // The receipt's storer was already recovered at the pushsync
                // decode boundary. The browser provider does not run the
                // full neighbourhood-depth verdict (it has no credible local
                // depth early in a session); a returned receipt is accepted as
                // proof the chunk reached a storer.
                return Ok(PushReceipt {
                    storer: receipt.storer,
                    signature: receipt.signature,
                    nonce: receipt.nonce,
                    storage_radius: receipt.storage_radius,
                });
            }
            Err(e) => {
                outcome = Err(SwarmError::AllPeersFailed {
                    address,
                    attempts,
                    source: Box::new(e),
                });
            }
        }
    }
    outcome
}
