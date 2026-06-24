//! Wasm-buildable proximity-routing chunk provider/sender for the browser client.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use nectar_primitives::SwarmAddress;
use vertex_swarm_api::{
    ChunkAddress, ChunkRetrievalResult, PushReceipt, StampedChunk, SwarmChunkProvider,
    SwarmChunkSender, SwarmError, SwarmResult, SwarmTopologyRouting, SwarmTopologyStats,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_node::{
    ChunkTransferError, ClientHandle, RaceFailure, race_candidates, retrieval_throttle_stats,
};
use vertex_swarm_primitives::OverlayAddress;
use vertex_swarm_topology::TopologyHandle;

/// Live per-peer in-flight retrieval-leg counts, for the load-concentration
/// histogram and least-loaded peer selection. Keyed by the peer we send the leg
/// to (the connected forwarder), not the chunk's storer. Measurement aid plus
/// the input to load-balanced selection.
static PEER_INFLIGHT: Mutex<Option<HashMap<OverlayAddress, u32>>> = Mutex::new(None);

fn peer_inflight_lock() -> std::sync::MutexGuard<'static, Option<HashMap<OverlayAddress, u32>>> {
    let mut g = PEER_INFLIGHT.lock().unwrap_or_else(|p| p.into_inner());
    if g.is_none() {
        *g = Some(HashMap::new());
    }
    g
}

/// Mark a leg in flight to `peer`, returning a guard that decrements on drop.
fn enter_inflight(peer: OverlayAddress) -> InflightGuard {
    if let Some(map) = peer_inflight_lock().as_mut() {
        *map.entry(peer).or_insert(0) += 1;
    }
    InflightGuard { peer }
}

struct InflightGuard {
    peer: OverlayAddress,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        if let Some(map) = peer_inflight_lock().as_mut()
            && let Some(c) = map.get_mut(&self.peer)
        {
            *c = c.saturating_sub(1);
            if *c == 0 {
                map.remove(&self.peer);
            }
        }
    }
}

/// Snapshot the in-flight concentration: `(peers_with_load, max_on_one_peer,
/// total_inflight, top10pct_share_x100)`. The last is the fraction of in-flight
/// legs held by the busiest 10% of loaded peers, scaled by 100, the headline
/// concentration figure (case 1 vs even spread).
fn inflight_concentration() -> (usize, u32, u32, u32) {
    let g = peer_inflight_lock();
    let Some(map) = g.as_ref() else {
        return (0, 0, 0, 0);
    };
    if map.is_empty() {
        return (0, 0, 0, 0);
    }
    let mut counts: Vec<u32> = map.values().copied().collect();
    counts.sort_unstable_by(|a, b| b.cmp(a));
    let total: u32 = counts.iter().sum();
    let peers = counts.len();
    let max = counts.first().copied().unwrap_or(0);
    let top10 = (peers / 10).max(1);
    let top_sum: u32 = counts.iter().take(top10).sum();
    let share = if total > 0 {
        (top_sum * 100) / total
    } else {
        0
    };
    (peers, max, total, share)
}

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

/// Load-balanced retrieval: assign each chunk to the least-in-flight peer drawn
/// from the closest `LB_TOP_K`, then add one hedge leg after `LB_HEDGE_MS`. This
/// keeps legs/chunk near 1-2 and spreads load evenly across all connected peers
/// instead of piling the closest few past their in-flight cap. On by default;
/// `lb=0` restores the widening-wave race for an A/B.
///
/// Sized to the prefetch fan-out: a wide download issues `DEFAULT_PREFETCH_CONCURRENCY`
/// concurrent retrievals against this pool, and each pool peer admits only
/// `MAX_INFLIGHT_PER_PEER` before bouncing the rest as `Busy`. A top-K below
/// `fan-out / per-peer-cap` oversubscribes the pool, so the surplus requests spin
/// the all-busy re-race (substreams-per-chunk climbs sharply and connections
/// churn as the wasted legs reset streams). A top-K of 32 gives 32 * 8 = 256
/// concurrent slots, matching the 256-wide prefetch, which roughly halves the
/// measured substreams-per-chunk and the connection churn versus a narrow pool.
const DEFAULT_LB_TOP_K: u64 = 32;
const DEFAULT_LB_HEDGE_MS: u64 = 1200;
static LB_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
static LB_TOP_K: AtomicU64 = AtomicU64::new(DEFAULT_LB_TOP_K);
static LB_HEDGE_MS: AtomicU64 = AtomicU64::new(DEFAULT_LB_HEDGE_MS);

/// Override the load-balanced retrieval knobs from the page URL (`lb`, `lbtopk`,
/// `lbhedge`). Measurement aid; absent params keep the defaults.
pub fn configure_load_balance(enabled: Option<bool>, top_k: Option<u64>, hedge_ms: Option<u64>) {
    if let Some(v) = enabled {
        LB_ENABLED.store(v, Ordering::Relaxed);
    }
    if let Some(v) = top_k.filter(|v| *v > 0) {
        LB_TOP_K.store(v, Ordering::Relaxed);
    }
    if let Some(v) = hedge_ms.filter(|v| *v > 0) {
        LB_HEDGE_MS.store(v, Ordering::Relaxed);
    }
}

fn lb_enabled() -> bool {
    LB_ENABLED.load(Ordering::Relaxed)
}
fn lb_top_k() -> usize {
    LB_TOP_K.load(Ordering::Relaxed) as usize
}
fn lb_hedge() -> Duration {
    Duration::from_millis(LB_HEDGE_MS.load(Ordering::Relaxed))
}

/// Order `ranked` (proximity-ranked) by current in-flight load ascending, so the
/// least-loaded close peer leads. Ties keep proximity order (stable sort).
fn least_loaded_first(ranked: &[OverlayAddress]) -> Vec<OverlayAddress> {
    let g = peer_inflight_lock();
    let load =
        |p: &OverlayAddress| -> u32 { g.as_ref().and_then(|m| m.get(p).copied()).unwrap_or(0) };
    let mut out = ranked.to_vec();
    out.sort_by_key(|p| load(p));
    out
}

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
/// Total winning-leg latency (ms) and count, to size per-chunk forwarding RTT
/// against the in-flight pool: the pool width over this latency caps throughput.
static HIT_LATENCY_MS_SUM: AtomicU64 = AtomicU64::new(0);
static HIT_LATENCY_CALLS: AtomicU64 = AtomicU64::new(0);

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

        // Load-balanced path: send each chunk to the least-in-flight peer among
        // the closest top-K (plus one delayed hedge), so legs/chunk stays near
        // 1-2 and load spreads evenly instead of piling the closest few past
        // their in-flight cap. The widening-wave race stays available via `lb=0`.
        let race = |ranked: &[OverlayAddress]| {
            let ranked = ranked.to_vec();
            async move {
                if lb_enabled() {
                    let top_k = lb_top_k().min(ranked.len());
                    let pool = least_loaded_first(&ranked[..top_k]);
                    let outcome = self.race_load_balanced(&pool, chunk_address).await;
                    // Fall back to the full widening-wave race when the two
                    // least-loaded peers neither held nor forwarded the chunk:
                    // the load-balanced pair is a fast common-case path, not a
                    // coverage guarantee, so a real miss still walks every close
                    // peer before failing the chunk. Busy stays a re-pick signal
                    // for the caller, and a hit returns straight away.
                    match outcome {
                        WaveOutcome::Hit(_)
                        | WaveOutcome::NoCandidates
                        | WaveOutcome::Failed(ChunkTransferError::Busy) => outcome,
                        WaveOutcome::NotFound | WaveOutcome::Failed(_) => {
                            self.race_connected_waves(&ranked, chunk_address).await
                        }
                    }
                } else {
                    self.race_connected_waves(&ranked, chunk_address).await
                }
            }
        };

        // Re-pick on an all-busy wave: every chosen peer was momentarily at its
        // in-flight cap (a wide parallel download), which is transient
        // back-pressure, not absence. Wait a short backoff and re-pick from the
        // refreshed load picture.
        let mut outcome = race(&ranked).await;
        for _ in 0..retrieve_busy_retries() {
            if !matches!(&outcome, WaveOutcome::Failed(ChunkTransferError::Busy)) {
                break;
            }
            futures_timer::Delay::new(RETRIEVE_BUSY_BACKOFF).await;
            outcome = race(&ranked).await;
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
                    let hit_lat_sum = HIT_LATENCY_MS_SUM.load(Ordering::Relaxed);
                    let hit_lat_calls = HIT_LATENCY_CALLS.load(Ordering::Relaxed).max(1);
                    let hit_lat_mean = hit_lat_sum / hit_lat_calls;
                    // Decompose the leg wall time: throttle-wait (allowance
                    // pacing, off-wire) vs the remaining on-wire RTT. The full
                    // leg latency is timed by the caller; the throttle reports
                    // its own pacing wait, so RTT = full - throttle-wait.
                    let (thr_wait_us, thr_calls, thr_capped, thr_sleep_us, thr_paced) =
                        retrieval_throttle_stats();
                    let thr_calls_nz = thr_calls.max(1);
                    let throttle_wait_ms_mean = (thr_wait_us / thr_calls_nz) / 1000;
                    // Intended allowance sleep (the bucket's wait hints) vs the
                    // wall above: wall minus this is single-thread executor
                    // backlog, separating true pacing from scheduling delay.
                    let throttle_sleep_ms_mean = (thr_sleep_us / thr_calls_nz) / 1000;
                    let rtt_ms_mean = hit_lat_mean.saturating_sub(throttle_wait_ms_mean);
                    // Concentration of in-flight legs across forwarder peers.
                    let (conc_peers, conc_max, conc_total, conc_top10_x100) =
                        inflight_concentration();
                    // Topology health over the download: the total connected-peer
                    // count (does the serving set hold or decay), the routing-table
                    // size, in-flight dials replenishing it, and the stored hive
                    // candidate pool the dialer draws from. The connected timeline is
                    // the proof of whether the peer set survives the whole download.
                    let topo_connected = self.topology.connected_peers_count();
                    let topo_routing = self.topology.routing_peers_count();
                    let topo_pending = self.topology.pending_connections_count();
                    let topo_stored = self.topology.stored_peers_count();
                    tracing::info!(
                        "retrieval-instrumentation served={served} legs={legs} \
                         substreams_per_chunk={spc} closest_to_us_mean={} closest_to_calls={ct_calls} \
                         leg_remote={remote} leg_notfound={notfound} leg_timeout={timeout} \
                         leg_protocol={protocol} leg_notconn={notconn} \
                         leg_cancelled={cancelled} leg_chanclosed={chanclosed} leg_busy={busy} \
                         hit_latency_ms_mean={hit_lat_mean} throttle_wait_ms_mean={throttle_wait_ms_mean} \
                         throttle_sleep_ms_mean={throttle_sleep_ms_mean} throttle_paced={thr_paced} \
                         rtt_ms_mean={rtt_ms_mean} throttle_capped={thr_capped} \
                         conc_peers={conc_peers} conc_max={conc_max} conc_inflight={conc_total} \
                         conc_top10_share={conc_top10_x100} \
                         topo_connected={topo_connected} topo_routing={topo_routing} \
                         topo_pending={topo_pending} topo_stored={topo_stored}",
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
// The `Hit` payload (a full chunk result) dwarfs the unit-ish failure variants,
// but this enum is a short-lived per-chunk return value moved straight into a
// match, never stored in bulk; boxing the hit would add a heap allocation on the
// retrieval hot path to shrink a stack temporary that never persists.
#[allow(clippy::large_enum_variant)]
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
    /// One retrieval leg to `peer`: tracks in-flight load, times the leg, tallies
    /// the per-outcome instrumentation, and returns the transfer result. Shared by
    /// the widening-wave race and the load-balanced path.
    async fn retrieve_leg(
        &self,
        peer: OverlayAddress,
        chunk_address: SwarmAddress,
    ) -> Result<vertex_swarm_node::RetrievalResult, ChunkTransferError> {
        LEGS_DISPATCHED.fetch_add(1, Ordering::Relaxed);
        let peer_po = chunk_address.proximity(&peer).get();
        let _inflight = enter_inflight(peer);
        let started = js_sys::Date::now();
        let outcome = self.client.retrieve_chunk(peer, chunk_address, true).await;
        let latency_ms = js_sys::Date::now() - started;
        match &outcome {
            Ok(_) => {
                HIT_LATENCY_MS_SUM.fetch_add(latency_ms as u64, Ordering::Relaxed);
                HIT_LATENCY_CALLS.fetch_add(1, Ordering::Relaxed);
            }
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
                %peer, chunk = %chunk_address, peer_po, latency_ms,
                "retrieval attempt: Hit"
            ),
            Err(ChunkTransferError::NotFound(_)) => tracing::debug!(
                %peer, chunk = %chunk_address, peer_po, latency_ms,
                "retrieval attempt: NotFound"
            ),
            Err(ChunkTransferError::TimedOut) => tracing::debug!(
                %peer, chunk = %chunk_address, peer_po, latency_ms,
                "retrieval attempt: Timeout"
            ),
            Err(e) => tracing::debug!(
                %peer, chunk = %chunk_address, peer_po, latency_ms, error = %e,
                "retrieval attempt: transport-error"
            ),
        }
        outcome
    }

    /// Race the least-loaded close peers with a single delayed hedge.
    ///
    /// `pool` is the closest top-K connected peers ordered least-in-flight first.
    /// The first leg goes to the least-loaded peer immediately; one hedge leg
    /// follows after `lb_hedge()` if no answer has arrived. This keeps legs per
    /// chunk near 1-2 and, because every chunk picks the currently least-loaded
    /// peer, spreads load evenly across all connected peers rather than piling on
    /// the few closest. An all-busy outcome surfaces as `Failed(Busy)` so the
    /// caller re-picks from a refreshed load picture.
    async fn race_load_balanced(
        &self,
        pool: &[OverlayAddress],
        chunk_address: SwarmAddress,
    ) -> WaveOutcome {
        // Two distinct least-loaded peers: the primary and one hedge. The hedge
        // stagger gives the primary a full RTT before the second leg opens, so a
        // healthy primary needs only one substream.
        let slice: Vec<_> = pool.iter().take(2).copied().collect();
        if slice.is_empty() {
            return WaveOutcome::NoCandidates;
        }
        match race_candidates(slice, lb_hedge(), |peer| {
            self.retrieve_leg(peer, chunk_address)
        })
        .await
        {
            Ok(result) => WaveOutcome::Hit(ChunkRetrievalResult {
                chunk: result.chunk,
                stamp: result.stamp,
                served_by: result.peer,
            }),
            Err(RaceFailure::NoCandidates) => WaveOutcome::NoCandidates,
            Err(RaceFailure::AllFailed(ChunkTransferError::NotFound(a))) => {
                let _ = a;
                WaveOutcome::NotFound
            }
            Err(RaceFailure::AllFailed(e)) => WaveOutcome::Failed(e),
        }
    }

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
                self.retrieve_leg(peer, chunk_address)
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
