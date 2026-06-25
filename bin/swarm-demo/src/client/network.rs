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
    ChunkTransferError, ClientHandle, RaceFailure, pseudosettle_stats, race_candidates,
    retrieval_debt_stats, retrieval_throttle_stats,
};
use vertex_swarm_primitives::OverlayAddress;
use vertex_swarm_topology::TopologyHandle;

/// Live per-peer in-flight retrieval-leg counts, for the load-concentration
/// histogram and least-loaded peer selection. Keyed by the peer we send the leg
/// to (the connected forwarder), not the chunk's storer. Measurement aid plus
/// the input to load-balanced selection.
static PEER_INFLIGHT: Mutex<Option<HashMap<OverlayAddress, u32>>> = Mutex::new(None);

/// Peers whose most recent retrieval leg failed with a transport fault
/// (`NotConnected`/`Cancelled`/`ChannelClosed`/`Protocol`), keyed to the epoch-ms
/// at which their cooldown expires. While a peer is cooling, the scheduler routes
/// around it: a connection bee just io_reset for crossing its debt line is mid
/// teardown, so dispatching there only burns a `NotConnected` leg and the single
/// thread. The next chunk re-dispatches to a live peer instead of stalling behind
/// the dead one. Selection never drops below one live candidate: if cooling would
/// empty the span, the cooldown is ignored for that pick.
static PEER_COOLDOWN: Mutex<Option<HashMap<OverlayAddress, f64>>> = Mutex::new(None);

/// Cooldown applied to a peer whose leg just failed with a transport fault, in
/// milliseconds. `0` disables routing-around (the pre-cooldown behaviour). Tunable
/// via the `cooldown` URL param so a sweep needs no rebuild.
static PEER_COOLDOWN_MS: AtomicU64 = AtomicU64::new(DEFAULT_PEER_COOLDOWN_MS);

/// Default cooldown on a just-reset peer. Disabled (`0`): under the deep-leaf tail
/// a handful of close peers serve the remaining chunks, and parking a just-reset
/// one routes the next dispatch onto an even smaller live set, concentrating load
/// and provoking more debt resets (measured: more disconnects, weaker tail rate
/// than fast re-dispatch alone). Fast re-dispatch already recovers the reset's
/// lost in-flight work without the routing-around. Opt in via the `cooldown` URL
/// param to experiment.
const DEFAULT_PEER_COOLDOWN_MS: u64 = 0;

/// Record `peer` as just-failed-with-transport-fault: route around it until the
/// cooldown elapses. No-op when the cooldown is disabled.
fn mark_peer_reset(peer: OverlayAddress) {
    let ms = PEER_COOLDOWN_MS.load(Ordering::Relaxed);
    if ms == 0 {
        return;
    }
    let expiry = js_sys::Date::now() + ms as f64;
    let mut g = PEER_COOLDOWN.lock().unwrap_or_else(|p| p.into_inner());
    g.get_or_insert_with(HashMap::new).insert(peer, expiry);
}

/// True if `peer` is within its post-reset cooldown window right now.
fn peer_is_cooling(peer: &OverlayAddress) -> bool {
    if PEER_COOLDOWN_MS.load(Ordering::Relaxed) == 0 {
        return false;
    }
    let now = js_sys::Date::now();
    let mut g = PEER_COOLDOWN.lock().unwrap_or_else(|p| p.into_inner());
    let Some(map) = g.as_mut() else {
        return false;
    };
    match map.get(peer).copied() {
        Some(expiry) if expiry > now => true,
        Some(_) => {
            map.remove(peer);
            false
        }
        None => false,
    }
}

/// Set the just-reset cooldown window from the page URL (`cooldown`, ms; `0`
/// disables). Measurement aid so an A/B needs no rebuild.
pub fn configure_peer_cooldown(ms: Option<u64>) {
    if let Some(v) = ms {
        PEER_COOLDOWN_MS.store(v, Ordering::Relaxed);
    }
}

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

/// Distributed retrieval scheduler: assign each chunk to the closest connected
/// peer that admits right now (free in-flight slot and live allowance headroom),
/// skipping busy or over-allowance peers to the next-closest across the full
/// connected set. A wide prefetch issues hundreds of these concurrently, so each
/// lands on a distinct closest-available peer and the load spreads across all
/// ~120-160 peers instead of piling the closest few past their accounting refresh
/// rate (the throughput ceiling the closest-only race hit). On by default; `lb=0`
/// restores the widening-wave race for an A/B.
static LB_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Select the distributed scheduler (`lb=1`, default) or the widening-wave race
/// (`lb=0`) from the page URL. The trailing two params are retained for URL
/// compatibility with earlier sweeps and ignored. Measurement aid.
pub fn configure_load_balance(enabled: Option<bool>, _top_k: Option<u64>, _hedge_ms: Option<u64>) {
    if let Some(v) = enabled {
        LB_ENABLED.store(v, Ordering::Relaxed);
    }
}

fn lb_enabled() -> bool {
    LB_ENABLED.load(Ordering::Relaxed)
}

/// The least-in-flight peer in `peers` (ties broken toward the front, i.e. the
/// closer peer). Used to spread the all-busy pacing leg across a saturated close
/// neighbourhood rather than serialising it onto the single closest peer.
fn least_loaded(peers: &[OverlayAddress]) -> Option<OverlayAddress> {
    let g = peer_inflight_lock();
    let load =
        |p: &OverlayAddress| -> u32 { g.as_ref().and_then(|m| m.get(p).copied()).unwrap_or(0) };
    peers.iter().copied().min_by_key(|p| load(p))
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

/// Per-served-bin tail diagnostic: a chunk's *whole-retrieval* wall time (entry
/// to hit, spanning every busy re-pick and the widening fall-through) and the
/// number of re-pick rounds it took, bucketed by how many chunks had already
/// been served when it completed. Bins of [`TAIL_BIN`] served chunks let the
/// instrumentation line show whether the last bins (the deep-leaf tail) cost
/// more wall time and more rounds per chunk than the early spread, which
/// distinguishes a scheduling-order tail from a retry/accounting-bound one.
/// Measurement aid, not a shipping metric.
const TAIL_BINS: usize = 8;
const TAIL_BIN: u64 = 125;
static TAIL_WALL_MS_SUM: [AtomicU64; TAIL_BINS] = [const { AtomicU64::new(0) }; TAIL_BINS];
static TAIL_ROUNDS_SUM: [AtomicU64; TAIL_BINS] = [const { AtomicU64::new(0) }; TAIL_BINS];
static TAIL_COUNT: [AtomicU64; TAIL_BINS] = [const { AtomicU64::new(0) }; TAIL_BINS];

/// Record one served chunk's whole-retrieval cost into its served-progress bin.
fn record_tail(served: u64, wall_ms: u64, rounds: u64) {
    let bin = ((served.saturating_sub(1)) / TAIL_BIN) as usize;
    let bin = bin.min(TAIL_BINS - 1);
    TAIL_WALL_MS_SUM[bin].fetch_add(wall_ms, Ordering::Relaxed);
    TAIL_ROUNDS_SUM[bin].fetch_add(rounds, Ordering::Relaxed);
    TAIL_COUNT[bin].fetch_add(1, Ordering::Relaxed);
}

/// Render the per-bin `mean_wall_ms/mean_rounds` series for the instrumentation
/// line, e.g. `bin0=12ms/1.1 bin1=...`. Empty bins are skipped.
fn tail_histogram() -> String {
    let mut parts = Vec::new();
    for b in 0..TAIL_BINS {
        let c = TAIL_COUNT[b].load(Ordering::Relaxed);
        if c == 0 {
            continue;
        }
        let wall = TAIL_WALL_MS_SUM[b].load(Ordering::Relaxed) / c;
        let rounds =
            (TAIL_ROUNDS_SUM[b].load(Ordering::Relaxed) as f64 / c as f64 * 100.0).round() / 100.0;
        parts.push(format!("b{b}={wall}ms/{rounds}r"));
    }
    parts.join(" ")
}

/// Hard ceiling on connected peers a retrieval considers before it stops
/// widening.
///
/// A retrieval never dials: it relies on the serving peer to forward the request
/// toward the chunk's neighbourhood (the retrieval protocol is forwarding-based).
/// The distributed scheduler walks this many closest connected peers, assigning
/// each chunk to the first that admits, so the budget is sized to span the whole
/// connected set (a live browser session holds ~120-160 peers) rather than the
/// closest few: spreading the prefetch fan-out across the full set is what lifts
/// throughput off the closest-peer accounting ceiling. Tunable via the `budget`
/// URL param.
const DEFAULT_RETRIEVE_CLOSE_BUDGET: usize = 256;

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

/// Real (admitted, substream-opening) legs the distributed scheduler tries for
/// one chunk before it stops widening. Bounds the substreams-per-chunk cost so a
/// chunk whose close peers forward-but-cannot-serve does not walk the whole set
/// opening streams; a genuinely absent chunk still surfaces as `NotFound` after
/// this many real attempts.
const ASSIGN_LEG_BUDGET: usize = 6;

/// Closest peers the distributed scheduler probes for non-blocking admission
/// before it concludes the neighbourhood is over its allowance and paces.
///
/// Spreading wins by landing each concurrent chunk on a distinct close-and-idle
/// peer, which only needs a probe span wide enough to cover the close peers a
/// wide prefetch fans onto, not the whole connected set: probing every peer when
/// most are over their allowance just burns the single thread on admission
/// bounces. This span is sized to the prefetch fan-out so concurrent chunks find
/// distinct idle peers, while an all-busy span falls through to the pacing leg.
const ASSIGN_PROBE_SPAN: usize = 48;

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

        // Distributed scheduler (default): assign the chunk to the closest
        // connected peer that admits *right now* (free in-flight slot and live
        // allowance headroom), walking closest-to-farthest across the full
        // connected set and skipping any busy or over-allowance peer rather than
        // waiting on it. A wide prefetch issues hundreds of these concurrently,
        // so each lands on a distinct closest-available peer and the load spreads
        // across the whole connected set instead of piling on the closest few
        // (whose accounting refresh caps throughput). The older least-loaded
        // top-K race and the widening-wave race stay available via `lb=0`.
        let race = |ranked: &[OverlayAddress]| {
            let ranked = ranked.to_vec();
            async move {
                if lb_enabled() {
                    let outcome = self.assign_closest_available(&ranked, chunk_address).await;
                    // Coverage fall-through: the distributed scheduler bounds the
                    // real legs it spends per chunk to keep the common case cheap,
                    // so a hard deep-leaf chunk (its close peers forward but no
                    // reachable peer holds it within the leg budget) can report a
                    // premature `NotFound`/failure. Before surfacing that, walk
                    // every candidate via the widening-wave race so a chunk only
                    // fails after the whole connected set was tried. A hit or a
                    // back-pressure (`Busy`) signal returns straight away.
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
        //
        // Time the whole retrieval (entry to hit) and count the re-pick rounds so
        // the tail diagnostic can attribute a slow tail to wall time vs round
        // count per served-progress bin.
        let chunk_started = js_sys::Date::now();
        let mut rounds = 1u64;
        let mut outcome = race(&ranked).await;
        for _ in 0..retrieve_busy_retries() {
            if !matches!(&outcome, WaveOutcome::Failed(ChunkTransferError::Busy)) {
                break;
            }
            futures_timer::Delay::new(RETRIEVE_BUSY_BACKOFF).await;
            rounds += 1;
            outcome = race(&ranked).await;
        }

        match outcome {
            WaveOutcome::Hit(result) => {
                let served = CHUNKS_SERVED.fetch_add(1, Ordering::Relaxed) + 1;
                record_tail(served, (js_sys::Date::now() - chunk_started) as u64, rounds);
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
                    // Outbound pseudosettle effectiveness: offers actually sent,
                    // and how much forgiveness the creditors granted (full vs
                    // partial acks). A partial-heavy ratio means settlement is
                    // claiming all the per-peer forgiveness the creditor will
                    // grant, i.e. we are pinned at the refresh-rate ceiling.
                    let (ps_offers, ps_offered_au, ps_accepted_au, ps_full, ps_partial) =
                        pseudosettle_stats();
                    // Debt-gate proof: the maximum per-peer unsettled debt the
                    // admission gate has observed (must stay below the remote's
                    // light disconnect line of 1,687,500 AU) and how many
                    // admissions the gate refused to bound that debt.
                    let (max_peer_debt, debt_gated) = retrieval_debt_stats();
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
                    let tail_hist = tail_histogram();
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
                         topo_pending={topo_pending} topo_stored={topo_stored} \
                         ps_offers={ps_offers} ps_offered_au={ps_offered_au} \
                         ps_accepted_au={ps_accepted_au} ps_full={ps_full} ps_partial={ps_partial} \
                         max_peer_debt={max_peer_debt} debt_gated={debt_gated} \
                         tail_hist=[{tail_hist}]",
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
    /// the widening-wave race and the load-balanced path. Uses the throttle's
    /// blocking admission (paces against a momentarily empty allowance bucket).
    async fn retrieve_leg(
        &self,
        peer: OverlayAddress,
        chunk_address: SwarmAddress,
    ) -> Result<vertex_swarm_node::RetrievalResult, ChunkTransferError> {
        self.retrieve_leg_inner(peer, chunk_address, false).await
    }

    /// One retrieval leg with non-blocking admission: a peer at its in-flight cap
    /// or momentarily without allowance headroom returns `Busy` at once instead
    /// of pacing, so the distributed scheduler can skip it to the next-closest
    /// peer.
    ///
    /// A `Busy` admission opened no substream, so it takes the cheap path,
    /// skipping the per-leg timing and inflight-guard bookkeeping: the scheduler
    /// probes up to `ASSIGN_PROBE_SPAN` peers per chunk and a wide prefetch runs
    /// hundreds of chunks at once, so charging the full instrumentation on every
    /// bounce would starve the single wasm thread under the tail's all-busy
    /// probing. Only an admitted leg (one that opened a substream) is timed.
    async fn try_retrieve_leg(
        &self,
        peer: OverlayAddress,
        chunk_address: SwarmAddress,
    ) -> Result<vertex_swarm_node::RetrievalResult, ChunkTransferError> {
        self.retrieve_leg_inner(peer, chunk_address, true).await
    }

    async fn retrieve_leg_inner(
        &self,
        peer: OverlayAddress,
        chunk_address: SwarmAddress,
        non_blocking: bool,
    ) -> Result<vertex_swarm_node::RetrievalResult, ChunkTransferError> {
        if non_blocking {
            // A non-blocking admission either bounces on `Busy` (no substream
            // opened) or admits and runs the leg. A bounce takes the cheap path:
            // bump only `LEG_BUSY` and return, skipping the inflight guard and
            // wall-clock timing so the tail's all-busy probing stays off the
            // single wasm thread's critical path. An admitted leg is timed and
            // tallied like any other.
            let _inflight = enter_inflight(peer);
            let started = js_sys::Date::now();
            let outcome = self
                .client
                .try_retrieve_chunk(peer, chunk_address, true)
                .await;
            if matches!(&outcome, Err(ChunkTransferError::Busy)) {
                LEG_BUSY.fetch_add(1, Ordering::Relaxed);
                return outcome;
            }
            let latency_ms = js_sys::Date::now() - started;
            return self.tally_leg(peer, chunk_address, latency_ms, outcome);
        }
        let _inflight = enter_inflight(peer);
        let started = js_sys::Date::now();
        let outcome = self.client.retrieve_chunk(peer, chunk_address, true).await;
        let latency_ms = js_sys::Date::now() - started;
        self.tally_leg(peer, chunk_address, latency_ms, outcome)
    }

    /// Tally one admitted leg's outcome (counters, hit latency, debug line) and
    /// return it unchanged. Shared by the blocking and non-blocking leg paths;
    /// never called for a `Busy` admission bounce, which opens no substream.
    fn tally_leg(
        &self,
        peer: OverlayAddress,
        chunk_address: SwarmAddress,
        latency_ms: f64,
        outcome: Result<vertex_swarm_node::RetrievalResult, ChunkTransferError>,
    ) -> Result<vertex_swarm_node::RetrievalResult, ChunkTransferError> {
        let peer_po = chunk_address.proximity(&peer).get();
        LEGS_DISPATCHED.fetch_add(1, Ordering::Relaxed);
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
                mark_peer_reset(peer);
            }
            Err(ChunkTransferError::NotConnected) => {
                LEG_NOTCONN.fetch_add(1, Ordering::Relaxed);
                mark_peer_reset(peer);
            }
            Err(ChunkTransferError::Cancelled) => {
                LEG_CANCELLED.fetch_add(1, Ordering::Relaxed);
                mark_peer_reset(peer);
            }
            Err(ChunkTransferError::ChannelClosed) => {
                LEG_CHANCLOSED.fetch_add(1, Ordering::Relaxed);
                mark_peer_reset(peer);
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

    /// Assign the chunk to the closest connected peer that admits right now.
    ///
    /// `ranked` is the full connected set ordered by proximity to the chunk. The
    /// scheduler walks it closest-to-farthest, probing each peer's non-blocking
    /// admission (free in-flight slot and live allowance headroom): a peer that
    /// would have to pace bounces on `Busy` and is skipped to the next-closest, so
    /// a wide concurrent prefetch spreads its fan-out across the whole connected
    /// set instead of piling on the closest few (whose accounting refresh caps
    /// throughput). The first peer that admits serves the leg.
    ///
    /// Two bounds keep this from degenerating into a probe storm under the
    /// accounting ceiling the model identifies as the binding constraint:
    ///
    /// - A *real* leg (one that admitted and opened a substream) that comes back
    ///   `NotFound` or with a transport fault is a genuine coverage miss, so the
    ///   walk tries the next admitting peer, but only up to `ASSIGN_LEG_BUDGET`
    ///   real legs before it stops widening: a chunk does not walk the whole set
    ///   opening substreams.
    /// - If the entire connected set is over its allowance (every probe bounced
    ///   on `Busy`), spreading cannot help: the aggregate forgiveness rate is the
    ///   bound. Rather than return `Busy` and spin the caller's re-pick, the chunk
    ///   paces: one *blocking* leg on the closest peer waits out that peer's
    ///   allowance bucket (the throttle's own forgiveness-rate pacing) and serves
    ///   the chunk. This is the open-loop steady state the model predicts.
    async fn assign_closest_available(
        &self,
        ranked: &[OverlayAddress],
        chunk_address: SwarmAddress,
    ) -> WaveOutcome {
        let Some((&closest, _)) = ranked.split_first() else {
            return WaveOutcome::NoCandidates;
        };
        let mut real_legs = 0usize;
        let mut admitted_any = false;
        let mut last_failure: Option<ChunkTransferError> = None;
        let probe_span = ranked.len().min(ASSIGN_PROBE_SPAN);
        // Route around peers whose connection just io_reset (mid teardown): they
        // would only return a `NotConnected` leg. Keep the proximity order. If
        // every peer in the span is cooling, fall through to the full span rather
        // than report no candidate, so a transient all-cooling window never fails
        // a chunk that a still-live peer could serve.
        let span = &ranked[..probe_span];
        let live: Vec<OverlayAddress> = span
            .iter()
            .copied()
            .filter(|p| !peer_is_cooling(p))
            .collect();
        let probe: &[OverlayAddress] = if live.is_empty() { span } else { &live };
        for &peer in probe {
            match self.try_retrieve_leg(peer, chunk_address).await {
                Ok(result) => {
                    return WaveOutcome::Hit(ChunkRetrievalResult {
                        chunk: result.chunk,
                        stamp: result.stamp,
                        served_by: result.peer,
                    });
                }
                // The peer would have to pace (full slot or empty bucket): skip
                // it to the next-closest without waiting. This is the spread.
                Err(ChunkTransferError::Busy) => continue,
                // The peer admitted but could not serve (forwarded-but-absent or a
                // transport fault): record it and walk onward to the next admitting
                // peer, up to the real-leg budget.
                Err(e) => {
                    admitted_any = true;
                    last_failure = Some(e);
                    real_legs += 1;
                    if real_legs >= ASSIGN_LEG_BUDGET {
                        break;
                    }
                }
            }
        }
        // Every probe bounced on Busy: the whole probe span is over its
        // allowance, so spreading is exhausted and the aggregate forgiveness rate
        // is the bound. Pace one blocking leg rather than spin the caller's
        // re-pick across an all-busy set. Pace on the *least-loaded* peer in the
        // span, not the closest: under the deep-leaf tail (chunks served by a
        // small close neighbourhood) the closest peer is the most contended, so
        // pacing there serialises the tail onto one bucket. Pacing on the
        // least-in-flight close peer spreads the wait across the neighbourhood's
        // buckets, holding the tail at the neighbourhood's aggregate forgiveness
        // rate instead of one peer's.
        if !admitted_any {
            let pace_peer = least_loaded(probe).unwrap_or(closest);
            return match self.retrieve_leg(pace_peer, chunk_address).await {
                Ok(result) => WaveOutcome::Hit(ChunkRetrievalResult {
                    chunk: result.chunk,
                    stamp: result.stamp,
                    served_by: result.peer,
                }),
                Err(ChunkTransferError::NotFound(_)) => WaveOutcome::NotFound,
                Err(e) => WaveOutcome::Failed(e),
            };
        }
        match last_failure {
            Some(ChunkTransferError::NotFound(_)) => WaveOutcome::NotFound,
            Some(e) => WaveOutcome::Failed(e),
            None => WaveOutcome::NotFound,
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
