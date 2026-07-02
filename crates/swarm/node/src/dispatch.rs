//! Shared dispatch engine for origin chunk retrieval and pushsync.
//!
//! Both the native and browser chunk providers build a [`DispatchEngine`] and
//! delegate their `retrieve_chunk` and push implementations to it, so the
//! bin-route primary, staggered refilling race, sequential push walk, in-flight
//! cap, adaptive stagger, headroom spill, and over-fetch metrics live in one
//! place. Each capability is a small trait the engine is generic over: the
//! native client supplies [`PeerSelector`], [`PeerInflightLimiter`], and
//! [`RetrievalLatency`]; the browser supplies the zero-sized null objects
//! [`ProximityOnly`] and [`NoLatencyHint`] but the same real
//! [`PeerInflightLimiter`].
//!
//! Every phase is built through the [`RaceBounds`] constructors, which carry
//! the discipline-to-commit-point rule: racing dispatch requires
//! commit-at-dispatch, only sequential dispatch may pair with an on-verify
//! commit.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use metrics::{counter, histogram};
use nectar_primitives::SwarmAddress;
use tokio::sync::OwnedSemaphorePermit;
use tracing::warn;
use vertex_swarm_api::{
    Bin, ChunkAddress, ChunkRetrievalResult, NeighborhoodDepth, OverlayAddress, PeerReporter,
    ReportSource, StampedChunk, SwarmError, SwarmResult, SwarmScoringEvent, SwarmTopologyPeers,
    SwarmTopologyReporting, SwarmTopologyRouting, SwarmTopologyState,
};
use vertex_swarm_net_pushsync::{DepthVerdict, Receipt};
use vertex_tasks::time::Duration;

use crate::retrieval_latency::{RetrievalLatency, adaptive_stagger};
use crate::selection::SettlementTrigger;
use crate::{
    ChunkTransferError, ClientHandle, PeerInflightLimiter, PeerSelector, RaceFailure,
    RetrievalResult, race_with_refill,
};

/// Proximity-ordered pool of closest connected peers the refilling race draws
/// from.
///
/// Wider than [`RETRIEVE_ATTEMPT_BUDGET`] so that, after the in-flight cap
/// removes peers at their limit, the race still has next-closest free-slot entry
/// points to refill a failed attempt rather than overrunning a hot peer.
/// Retrieval is forwarding-based, so these are connected peers only; the race
/// never dials.
const RETRIEVE_WIDTH: usize = 32;

/// Maximum real retrieval attempts the race dispatches per chunk before giving
/// up.
///
/// A chunk whose closest few entry points miss is not absent: each attempt is a
/// distinct peer whose own forwarding chain may still reach the chunk's storers,
/// so a sparse-bin chunk needs more than the closest few tries. Peers skipped
/// for back-pressure before dispatch do not count against this budget, so it
/// bounds only genuine coverage attempts and the metered bandwidth they cost.
pub(crate) const RETRIEVE_ATTEMPT_BUDGET: usize = 8;

/// Maximum attempts the staggered fallback keeps concurrently in flight,
/// bounding the metered over-fetch independently of [`RETRIEVE_ATTEMPT_BUDGET`].
///
/// A losing race attempt is not cancelled downstream: the serving chain forwards
/// and delivers regardless, so every overlapping attempt is real duplicate
/// bandwidth and cost. This caps the simultaneous attempts at a handful while
/// [`RETRIEVE_ATTEMPT_BUDGET`] still bounds the lifetime total.
pub(crate) const RETRIEVE_MAX_IN_FLIGHT: usize = 3;

/// Wall-clock bound on the whole refilling race across its refilled attempts.
pub(crate) const RETRIEVE_DEADLINE: Duration = Duration::from_secs(30);

/// Wider topology slice the retrieval fallback spills to when the close set is
/// fully gated.
///
/// The closest peers to a chunk carry a download's debt and gate first; a far
/// larger set of slightly-farther peers still holds forgiveness headroom and
/// forwards the chunk just the same.
const RETRIEVE_SPILL_WIDTH: usize = 128;

/// Attempt budget for the farther-ring spill phase: the second race a difficult
/// chunk falls to when its whole close set fails on the wire.
///
/// Bounded on its own so the two phases' combined metered fan-out stays a small
/// multiple, never a runaway. The alternative to spending it is failing the
/// chunk outright and having the consumer re-stream, which costs more attempts
/// still.
const RETRIEVE_SPILL_BUDGET: usize = RETRIEVE_ATTEMPT_BUDGET;

/// Wall-clock bound on the farther-ring spill race, separate from the close
/// race's deadline so the spill phase cannot itself stall the pipeline slot.
const RETRIEVE_SPILL_DEADLINE: Duration = Duration::from_secs(15);

/// Closest peers a fully-gated retrieval drives a settle on before re-selecting.
///
/// When every close and spill peer is past its disconnect line the band yields no
/// candidate. The dispatch path settles only a peer it contacts, so a fully-gated
/// set never drains on its own: nothing dispatches, nothing settles, the debt
/// stays pinned at the line. Driving a settle on the closest gated peers reopens
/// the band. Matches the spill ring rather than the close set: a debt-saturated
/// bulk download reopens more headroom the more peers forgive in parallel, and
/// the trigger's in-flight dedup (shared with the origin gate) collapses
/// concurrent gated retrievals to one settle per peer, so a wider drive unlocks
/// more aggregate forgiveness without multiplying the settle traffic.
const RETRIEVE_SETTLE_DRIVE_WIDTH: usize = RETRIEVE_SPILL_WIDTH;

/// Backoff between settle drives, giving a spawned pseudosettle a round trip to
/// land and reopen the band before the fallback re-selects.
const RETRIEVE_SETTLE_DRIVE_BACKOFF: Duration = Duration::from_millis(250);

/// Settle drives a fully-gated retrieval attempts before giving up and letting
/// the consumer re-stream. Bounds the added latency (`WIDTH * BACKOFF`) so a
/// genuinely peerless node still terminates rather than parking the pipeline slot
/// for the whole retrieval deadline.
const RETRIEVE_SETTLE_DRIVES: usize = 10;

/// Attempts the bin-bucket primary route dispatches before handing off to the
/// staggered fallback.
///
/// The primary routes a chunk to its Kademlia forwarding bin and tries the
/// in-headroom connected peers there one at a time. Past that, the difficult
/// chunk is better served by the wider staggered fallback than by more
/// single-flight bin tries.
const PRIMARY_ROUTE_BUDGET: usize = 4;

/// Wall-clock bound on the whole bin-bucket primary route.
///
/// The primary is single-flight, so a withholding head holds the one in-flight
/// attempt with no overlapping attempt to overtake it. This deadline hands a
/// stuck route off to the staggered fallback rather than stalling the chunk's
/// pipeline slot.
const PRIMARY_ROUTE_DEADLINE: Duration = Duration::from_secs(2);

/// Stagger that never fires within any deadline: sequential phases are strict
/// single-flight, so the staggered fan-out is reserved for racing phases.
const NEVER_STAGGER: Duration = Duration::from_secs(3600);

/// Number of closest peers to try when pushing a chunk before giving up.
const PUSH_CANDIDATE_COUNT: usize = 5;

/// Report source for shallow receipts caught on the origin upload path.
const PUSHSYNC_SOURCE: ReportSource = ReportSource::Protocol("pushsync");

/// Topology surface the retrieval engine and native push path read.
///
/// A marker bundling the four topology query traits dispatch needs into one
/// object-safe trait, so the engine holds a single `Arc<dyn RetrievalTopology>`
/// rather than a generic per identity type. Every method it calls belongs to a
/// sub-trait; the one non-trait value the engine needs, the routing table's max
/// bin, is a construction constant carried as an engine field, not a query. The
/// blanket impl means the node's real handle and a test mock both qualify with no
/// bespoke impl.
pub trait RetrievalTopology:
    SwarmTopologyState + SwarmTopologyRouting + SwarmTopologyPeers + SwarmTopologyReporting
{
}

impl<T> RetrievalTopology for T where
    T: SwarmTopologyState + SwarmTopologyRouting + SwarmTopologyPeers + SwarmTopologyReporting
{
}

/// Economic ordering of retrieval and pushsync candidates.
///
/// The native client supplies [`PeerSelector`] (score- and affordability-aware);
/// the browser supplies [`ProximityOnly`], which leaves the proximity order
/// untouched.
#[auto_impl::auto_impl(&, Arc)]
pub trait CandidateOrdering: Send + Sync {
    /// Order `candidates` for a request on `chunk` (band- and score-aware).
    fn order(&self, candidates: Vec<OverlayAddress>, chunk: &ChunkAddress) -> Vec<OverlayAddress>;

    /// Order by closeness among the admissible, dropping the headroom tiering;
    /// the close spill uses this to travel the fewest forwarding hops.
    fn order_closest_admissible(
        &self,
        candidates: Vec<OverlayAddress>,
        chunk: &ChunkAddress,
    ) -> Vec<OverlayAddress>;
}

impl CandidateOrdering for PeerSelector {
    fn order(&self, candidates: Vec<OverlayAddress>, chunk: &ChunkAddress) -> Vec<OverlayAddress> {
        PeerSelector::order(self, candidates, chunk)
    }

    fn order_closest_admissible(
        &self,
        candidates: Vec<OverlayAddress>,
        chunk: &ChunkAddress,
    ) -> Vec<OverlayAddress> {
        PeerSelector::order_closest_admissible(self, candidates, chunk)
    }
}

/// Per-peer cap on concurrent outbound retrieval substreams.
///
/// Supplied by [`PeerInflightLimiter`] on both node types: it bounds concurrent
/// outbound retrieval substreams per peer so a hot peer's multiplexer budget is
/// not overrun.
#[auto_impl::auto_impl(&, Arc)]
pub trait InflightLimit: Send + Sync {
    /// Per-attempt reservation that releases the slot on drop, including a
    /// cancelled losing attempt.
    type Permit: vertex_tasks::MaybeSend + 'static;

    /// Order proximity-ordered `candidates` free-slot peers first, busy peers as
    /// a tail, and report whether the race should enforce the cap. No candidate
    /// is dropped: a busy-but-good holder stays reachable as a last resort. The
    /// tail is only ever contacted after the free leaders fail, so the cap is not
    /// enforced (degraded service beats failing the request).
    fn available(&self, candidates: Vec<OverlayAddress>) -> (Vec<OverlayAddress>, bool);

    /// Reserve a slot for `peer`, or `None` when it is at its cap.
    fn try_acquire(&self, peer: &OverlayAddress) -> Option<Self::Permit>;
}

impl InflightLimit for PeerInflightLimiter {
    type Permit = OwnedSemaphorePermit;

    fn available(&self, candidates: Vec<OverlayAddress>) -> (Vec<OverlayAddress>, bool) {
        // Free-slot peers lead; busy peers are appended as a tail rather than
        // dropped. The staggered race dispatches the free leaders first and
        // resolves on the first success, so an easy chunk with a free holder never
        // reaches the tail (no extra over-fetch); only a difficult chunk whose
        // leaders all fail reaches the busy-but-good holders best-effort,
        // dispatching over their cap as the bounded last resort that actually
        // reaches the holder. Free-first ordering is what keeps the tail a last
        // resort, so the cap is never enforced.
        let (free, busy): (Vec<OverlayAddress>, Vec<OverlayAddress>) = candidates
            .into_iter()
            .partition(|peer| self.has_free_slot(peer));
        (free.into_iter().chain(busy).collect(), false)
    }

    fn try_acquire(&self, peer: &OverlayAddress) -> Option<Self::Permit> {
        PeerInflightLimiter::try_acquire(self, peer)
    }
}

/// Per-proximity-order latency estimate that paces the staggered race.
///
/// The native client supplies [`RetrievalLatency`]; the browser supplies
/// [`NoLatencyHint`], so the stagger falls back to the constant.
#[auto_impl::auto_impl(&, Arc)]
pub trait LatencyHint: Send + Sync {
    /// The latency estimate for proximity `po`, or `None` when unsampled.
    fn estimate(&self, po: u8) -> Option<Duration>;
}

impl LatencyHint for RetrievalLatency {
    fn estimate(&self, po: u8) -> Option<Duration> {
        RetrievalLatency::estimate(self, po)
    }
}

/// Proximity-only ordering: returns candidates unchanged, with no economic
/// signal. The browser client's [`CandidateOrdering`].
#[derive(Clone, Copy, Debug, Default)]
pub struct ProximityOnly;

impl CandidateOrdering for ProximityOnly {
    fn order(&self, candidates: Vec<OverlayAddress>, _chunk: &ChunkAddress) -> Vec<OverlayAddress> {
        candidates
    }

    fn order_closest_admissible(
        &self,
        candidates: Vec<OverlayAddress>,
        _chunk: &ChunkAddress,
    ) -> Vec<OverlayAddress> {
        candidates
    }
}

/// No latency estimate: the adaptive stagger falls back to the constant. The
/// browser client's [`LatencyHint`].
#[derive(Clone, Copy, Debug, Default)]
pub struct NoLatencyHint;

impl LatencyHint for NoLatencyHint {
    fn estimate(&self, _po: u8) -> Option<Duration> {
        None
    }
}

/// Tuning for one dispatch phase: the lifetime attempt `budget`, the
/// concurrent-attempt width `max_in_flight`, the wall-clock `deadline`, and the
/// per-attempt `stagger`.
///
/// The two constructors carry the discipline-to-commit-point rule: racing
/// dispatch requires commit-at-dispatch, sequential dispatch permits
/// commit-on-verify. Build a phase through them, never by literal.
struct RaceBounds {
    budget: usize,
    max_in_flight: usize,
    deadline: Duration,
    stagger: Duration,
}

impl RaceBounds {
    /// A staggered race of concurrent legs. Every raced leg must book its
    /// debit at dispatch: a losing leg is cancelled by drop and a dropped
    /// reservation releases, so a raced leg committing on verify could
    /// un-book debt for bytes the wire may still deliver.
    fn racing(budget: usize, max_in_flight: usize, deadline: Duration, stagger: Duration) -> Self {
        Self {
            budget,
            max_in_flight,
            deadline,
            stagger,
        }
    }

    /// One leg at a time: the stagger never fires and no leg is cancelled
    /// mid-flight, so only a sequential phase may ever pair with an on-verify
    /// commit (the relay profile consumes that freedom; every origin leg books
    /// at dispatch regardless).
    fn sequential(budget: usize, deadline: Duration) -> Self {
        Self {
            budget,
            max_in_flight: 1,
            deadline,
            stagger: NEVER_STAGGER,
        }
    }
}

/// Shared dispatch engine for origin chunk retrieval.
///
/// Generic over its three capabilities so a native client wires the concrete
/// providers and the browser wires zero-sized null objects. Both build one and
/// delegate `retrieve_chunk` to [`Self::retrieve`]. Every retrieval terminal
/// (no candidates, all attempts failed, deadline) maps to
/// [`SwarmError::RetrievalExhausted`]: forwarding retrieval has no authoritative
/// negative, so the engine never adjudicates absence.
#[derive(Clone)]
pub struct DispatchEngine<O: CandidateOrdering, G: InflightLimit, L: LatencyHint> {
    client_handle: ClientHandle,
    topology: Arc<dyn RetrievalTopology>,
    /// The routing table's highest bin, the ceiling for the forwarding-bin route.
    /// A spec constant fixed at construction, so it is a field, not a per-request
    /// topology query.
    max_bin: Bin,
    ordering: O,
    inflight: G,
    latency: L,
    /// Drains a fully-gated close/spill set: the fallback drives a settle on the
    /// closest refused peers to reopen the band. Shares the origin gate's
    /// in-flight dedup, so concurrent gated retrievals collapse to one settle per
    /// peer.
    settlement: Arc<dyn SettlementTrigger>,
}

impl<O, G, L> DispatchEngine<O, G, L>
where
    O: CandidateOrdering,
    G: InflightLimit,
    L: LatencyHint,
{
    /// Build an engine over its capabilities.
    pub fn new(
        client_handle: ClientHandle,
        topology: Arc<dyn RetrievalTopology>,
        max_bin: Bin,
        ordering: O,
        inflight: G,
        latency: L,
        settlement: Arc<dyn SettlementTrigger>,
    ) -> Self {
        Self {
            client_handle,
            topology,
            max_bin,
            ordering,
            inflight,
            latency,
            settlement,
        }
    }

    /// The topology, for the provider's local-cache serve labelling; dispatch
    /// reaches topology through the engine's own methods.
    pub(crate) fn topology(&self) -> &Arc<dyn RetrievalTopology> {
        &self.topology
    }

    /// Push `chunk` to the closest storers, returning the first custody
    /// receipt that verifies.
    ///
    /// The push profile is sequential origin dispatch: the closest
    /// [`PUSH_CANDIDATE_COUNT`] peers ranked through the accounting band, one
    /// leg at a time (the client handle correlates a push response to its
    /// request by chunk address alone, so legs are never raced), every leg
    /// booking at dispatch through the origin credit gate.
    pub async fn push(&self, chunk: StampedChunk) -> SwarmResult<Receipt> {
        let address = *chunk.address();
        let closest = self.topology.closest_to(&address, PUSH_CANDIDATE_COUNT);
        // Rank by band and score, hard-skipping a refused peer; an all-gated set
        // yields an empty result and the generic no-storer outcome below.
        let closest = self.ordering.order(closest, &address);
        let attempts = closest.len();

        // The required custody depth is derived from our locally observed
        // neighbourhood depth (the trusted authority) and trust-but-verified
        // against the receipt's own claimed `storage_radius`. The check is gated
        // on that depth being credible (the neighbourhood has saturated); a
        // non-credible view cannot anchor the floor and yields an unverifiable
        // verdict. The receipt's signer was already recovered at the decode
        // boundary; a malformed receipt never reaches here (it surfaces as a push
        // error below).
        let local_depth = self.topology.depth();
        let neighbourhood_credible = self.topology.neighbourhood_credible();
        let reporter = self.topology.reporter();

        // Try each closest peer in order and return the first receipt that
        // verifies. A shallow receipt is rejected, the responding peer scored
        // adversely, and the loop continues to the next candidate: this is the
        // retry-via-different-route dynamic the depth check exists to engage (a
        // fabricated shallow receipt no longer convinces the uploader the push
        // succeeded). An unverifiable receipt (non-credible local view) is also
        // not trusted, but the responder is NOT penalised: it may be honest, we
        // just cannot judge custody depth. If no candidate verifies and at least
        // one was unverifiable, the push is reported as unconfirmed custody
        // rather than a hard failure. The seed error covers the no-candidates
        // case; each attempt replaces it, so the value after the loop is the last
        // failure.
        let mut outcome = Err(SwarmError::NoStorer {
            chunk_address: address,
        });
        for peer in closest {
            // `originated = true`: our own push, so the client service debits
            // the storer on receipt.
            match self
                .client_handle
                .push_chunk(peer, chunk.clone(), true)
                .await
            {
                Ok(receipt) => {
                    match accept_origin_receipt(
                        &receipt,
                        peer,
                        local_depth,
                        neighbourhood_credible,
                        reporter.as_ref(),
                    ) {
                        DepthVerdict::Verified => return Ok(receipt),
                        DepthVerdict::Shallow(err) => {
                            outcome = Err(SwarmError::InvalidSignature {
                                chunk_address: address,
                                reason: err.to_string(),
                            });
                        }
                        DepthVerdict::Unverifiable => {
                            // Surface unconfirmed custody distinctly from a hard
                            // invalid-signature failure. A later shallow verdict
                            // (a proven finding) takes precedence over this; an
                            // earlier one is not downgraded.
                            if !matches!(outcome, Err(SwarmError::InvalidSignature { .. })) {
                                outcome = Err(SwarmError::UnconfirmedCustody {
                                    chunk_address: address,
                                });
                            }
                        }
                    }
                }
                Err(e) => {
                    // A transport-level failure is the weakest signal: it does
                    // not overwrite a depth verdict (shallow misbehaviour or
                    // unconfirmed custody) already recorded for an earlier
                    // candidate.
                    if !matches!(
                        outcome,
                        Err(SwarmError::InvalidSignature { .. })
                            | Err(SwarmError::UnconfirmedCustody { .. })
                    ) {
                        outcome = Err(SwarmError::AllPeersFailed {
                            address,
                            attempts,
                            source: Box::new(e),
                        });
                    }
                }
            }
        }

        outcome
    }

    /// Order the close set, spilling to the closest admissible peers of a wider
    /// slice when the close set is fully gated.
    ///
    /// A non-empty band over the close set returns at once (the fast path),
    /// keeping the close set's headroom-first debt spread. When every close peer
    /// is refused, the closest few have spent their forgiveness on this download
    /// while a far larger ring of slightly-farther peers still carries headroom;
    /// banding [`RETRIEVE_SPILL_WIDTH`] peers and routing the chunk to an
    /// admissible one there forwards it just the same, without parking the
    /// pipeline slot. If even the wider slice is fully gated the result is empty
    /// and the caller falls through to its terminal failure. With
    /// [`ProximityOnly`] the proximity order is returned unchanged.
    fn order_with_spill(
        &self,
        candidates: Vec<OverlayAddress>,
        chunk: &ChunkAddress,
    ) -> Vec<OverlayAddress> {
        let ordered = self.ordering.order(candidates, chunk);
        if !ordered.is_empty() {
            return ordered;
        }
        let wide = self.topology.closest_to(chunk, RETRIEVE_SPILL_WIDTH);
        self.ordering.order_closest_admissible(wide, chunk)
    }

    /// Run one bounded staggered race over `candidates`, dispatching each attempt
    /// as an originated retrieval that reserves the per-peer in-flight permit
    /// riding its future.
    ///
    /// With `enforce_cap`, a peer that filled its in-flight slot since the
    /// availability snapshot is declined at dispatch (no attempt, no budget unit)
    /// so the cap holds on live state; without it (no limiter, or the all-busy
    /// fall-through) the attempt runs best-effort even with no permit.
    async fn race_attempts(
        &self,
        candidates: Vec<OverlayAddress>,
        chunk_address: SwarmAddress,
        bounds: RaceBounds,
        enforce_cap: bool,
        attempts: &AtomicUsize,
    ) -> Result<RetrievalResult, RaceFailure<ChunkTransferError>> {
        race_with_refill(
            candidates,
            bounds.budget,
            bounds.max_in_flight,
            bounds.deadline,
            bounds.stagger,
            |peer_overlay| {
                let permit = self.inflight.try_acquire(&peer_overlay);
                // A peer that filled since the availability snapshot is declined
                // so the cap holds on live state, spending no budget; best-effort
                // (no limiter or all-busy fall-through) attempts without a permit.
                if enforce_cap && permit.is_none() {
                    return None;
                }
                attempts.fetch_add(1, Ordering::Relaxed);
                // `originated = true`: our own retrieval, so the client service
                // debits the serving peer on delivery.
                let request = self
                    .client_handle
                    .retrieve_chunk(peer_overlay, chunk_address, true);
                Some(async move {
                    let _permit = permit;
                    request.await
                })
            },
        )
        .await
    }

    /// Retrieve a chunk by the full dispatch policy.
    ///
    /// Runs the bin-route primary (single-flight, in-bin peers first), then the
    /// staggered bounded-refill fallback. Every retrieval terminal maps to
    /// [`SwarmError::RetrievalExhausted`]; the attempt count and last error stay
    /// in the metrics and debug log, never the error variant.
    pub async fn retrieve(&self, address: &ChunkAddress) -> SwarmResult<ChunkRetrievalResult> {
        let chunk_address = SwarmAddress::new(address.0.into());
        let attempts = AtomicUsize::new(0);

        // PRIMARY: bin-bucket proximity route. Route the chunk to its Kademlia
        // forwarding bin b = PO(local, chunk) and dispatch the best in-headroom
        // connected peer there, spilling to the adjacent bins on saturation. A
        // peer in bin b already shares the chunk's first b bits, so it forwards
        // over fewer hops than a peer picked by raw closeness alone: fewer hops
        // is lower per-chunk latency, the throughput lever. The route is
        // single-flight (no stagger), so the happy path delivers one chunk for
        // one metered attempt and over-fetches nothing. The accounting band ranks
        // the route here; a gated route falls through to the staggered fallback,
        // which spills to a wider headroom slice.
        let local = self.topology.overlay_address();
        let max_bin = self.max_bin.get();
        let bin_candidates =
            bin_routed_order(&chunk_address, &local, max_bin, RETRIEVE_WIDTH, |bin| {
                self.topology.connected_peers_in_bin(bin)
            });
        let bin_candidates = self.ordering.order(bin_candidates, address);
        // Availability: drop peers at their in-flight cap so the route leads with
        // an in-headroom peer. The cap is the non-economic muxer guard, composed
        // after the accounting band, never merged with it.
        let (bin_candidates, enforce_cap) = self.inflight.available(bin_candidates);

        if !bin_candidates.is_empty() {
            let primary = self
                .race_attempts(
                    bin_candidates,
                    chunk_address,
                    // Single-flight: at most one metered attempt in flight,
                    // advancing the bin spill only on an explicit failure.
                    RaceBounds::sequential(PRIMARY_ROUTE_BUDGET, PRIMARY_ROUTE_DEADLINE),
                    enforce_cap,
                    &attempts,
                )
                .await;
            if let Ok(result) = primary {
                let dispatched = attempts.load(Ordering::Relaxed);
                histogram!("swarm.client.retrieval_attempts").record(dispatched as f64);
                counter!("swarm.client.retrieval_total", "outcome" => "hit", "path" => "bin_route")
                    .increment(1);
                record_overfetch(dispatched, "bin_route");
                return Ok(ChunkRetrievalResult {
                    chunk: result.chunk,
                    stamp: result.stamp,
                    served_by: result.peer,
                });
            }
        }

        // FALLBACK: the staggered bounded-refill race over the globally closest
        // connected peers. Reached when the bin route is gated, saturated, or its
        // entry points all miss. Retrieval is forwarding-Kademlia with no
        // authoritative negative on the wire: a failed or slow attempt means
        // "this entry point could not serve it", never "the chunk is absent", so
        // the race keeps going and only gives up on a real bound (the attempt
        // budget, the pool, or the deadline). Staggering one attempt in at a time
        // bounds the paid fan-out; each attempt reserves the per-peer in-flight
        // permit that rides its future, released on drop including a cancelled
        // losing attempt.
        // A fully-gated set (every close and spill peer past its disconnect line)
        // yields no candidate to race. The dispatch path settles only a peer it
        // contacts, so a gated set never drains on its own: nothing dispatches,
        // nothing settles, the debt stays pinned at the line. Before giving up,
        // drive a settle on the closest gated peers and re-select, bounded so a
        // genuinely peerless node still terminates. The trigger's in-flight dedup
        // collapses the concurrent gated retrievals of a bulk download to one
        // settle per peer, so this drives the download at the peers' forgiveness
        // rate rather than spamming settles.
        let mut settle_drives = 0usize;
        let outcome = loop {
            let closest_peers = self.topology.closest_to(&chunk_address, RETRIEVE_WIDTH);
            // Spill to a wider in-headroom slice when every close peer is gated, so
            // a fully gated close set routes around its spent peers rather than
            // blocking.
            let closest_peers = self.order_with_spill(closest_peers, address);
            let (close_candidates, _enforce_cap) = self.inflight.available(closest_peers);

            // Farther-ring spill: when the whole close set fails on the wire (not
            // merely gated), widen to the admissible peers of a larger slice, minus
            // the close peers already raced, so the second race reaches holders
            // beyond the close set rather than re-racing the same failing peers. A
            // gated close set already spilled to this slice above, so its
            // already-raced set covers the slice and the difference is empty.
            let raced: HashSet<OverlayAddress> = close_candidates.iter().copied().collect();
            let wide = self
                .topology
                .closest_to(&chunk_address, RETRIEVE_SPILL_WIDTH);
            let wide = self.ordering.order_closest_admissible(wide, address);
            let spill_ring: Vec<OverlayAddress> = wide
                .into_iter()
                .filter(|peer| !raced.contains(peer))
                .collect();
            let (spill_candidates, _spill_enforce_cap) = self.inflight.available(spill_ring);

            if close_candidates.is_empty() && spill_candidates.is_empty() {
                // Either fully gated or genuinely peerless. Settle the closest raw
                // peers so their debt drains below the disconnect line, back off for
                // the settles to land, and re-select. A peerless node (no closest
                // peers) or an exhausted drive budget falls through to the terminal
                // no-candidates failure the consumer re-streams on.
                let gated = self
                    .topology
                    .closest_to(&chunk_address, RETRIEVE_SETTLE_DRIVE_WIDTH);
                if gated.is_empty() || settle_drives >= RETRIEVE_SETTLE_DRIVES {
                    break Err(RaceFailure::NoCandidates);
                }
                for peer in gated {
                    self.settlement.trigger_settlement(peer);
                }
                settle_drives += 1;
                counter!("swarm.client.retrieval_settle_drive").increment(1);
                // `futures_timer::Delay`, not `vertex_tasks::time::sleep`: the
                // latter is `!Send` on wasm and this future carries the async-trait
                // `Send` bound. `Delay` is the Send-safe timer the race staggers use.
                futures_timer::Delay::new(RETRIEVE_SETTLE_DRIVE_BACKOFF).await;
                continue;
            }

            // Pace each phase's staggered fan-out to the live round trip rather than
            // a fixed constant. Each candidate's expected latency is read from the
            // per-PO estimate at its proximity to the chunk (the forwarding distance
            // that dominates retrieval latency). Cold buckets fall back to the
            // constant, so this is never slower, only faster on low-RTT distances.
            let close_stagger = adaptive_stagger(
                close_candidates
                    .iter()
                    .map(|peer| self.latency.estimate(chunk_address.proximity(peer).get())),
            );
            let spill_stagger = adaptive_stagger(
                spill_candidates
                    .iter()
                    .map(|peer| self.latency.estimate(chunk_address.proximity(peer).get())),
            );

            // One dispatch closure feeds both phases. The in-flight cap is not
            // enforced here (see `available`): a busy-but-good holder is dispatched
            // best-effort, but only after the free leaders are exhausted, so the
            // easy chunk still serves from a free peer with no extra over-fetch. The
            // permit still rides each request future and releases on drop, including
            // a cancelled losing attempt.
            let dispatch = |peer_overlay: OverlayAddress| {
                let permit = self.inflight.try_acquire(&peer_overlay);
                attempts.fetch_add(1, Ordering::Relaxed);
                // `originated = true`: our own retrieval, so the client service
                // debits the serving peer on delivery.
                let request = self
                    .client_handle
                    .retrieve_chunk(peer_overlay, chunk_address, true);
                Some(async move {
                    let _permit = permit;
                    request.await
                })
            };

            break race_close_then_spill(
                close_candidates,
                spill_candidates,
                RaceBounds::racing(
                    RETRIEVE_ATTEMPT_BUDGET,
                    RETRIEVE_MAX_IN_FLIGHT,
                    RETRIEVE_DEADLINE,
                    close_stagger,
                ),
                RaceBounds::racing(
                    RETRIEVE_SPILL_BUDGET,
                    RETRIEVE_MAX_IN_FLIGHT,
                    RETRIEVE_SPILL_DEADLINE,
                    spill_stagger,
                ),
                dispatch,
            )
            .await;
        };

        let dispatched = attempts.load(Ordering::Relaxed);
        histogram!("swarm.client.retrieval_attempts").record(dispatched as f64);
        let outcome_label = match &outcome {
            Ok(_) => "hit",
            Err(RaceFailure::NoCandidates) => "no_peers",
            Err(RaceFailure::AllFailed(_)) => "exhausted",
            Err(RaceFailure::TimedOut) => "timed_out",
        };
        counter!("swarm.client.retrieval_total", "outcome" => outcome_label, "path" => "fallback")
            .increment(1);
        if outcome.is_ok() {
            // `dispatched` spans both phases, so a fallback win also counts the
            // primary attempts the missed bin route already spent.
            record_overfetch(dispatched, "fallback");
        }

        match outcome {
            Ok(result) => Ok(ChunkRetrievalResult {
                chunk: result.chunk,
                stamp: result.stamp,
                served_by: result.peer,
            }),
            // Forwarding retrieval has no authoritative negative, so every
            // terminal is the same honest outcome: the reachable peers were
            // exhausted without serving the chunk. The which-attempt and
            // last-error detail lives in the metrics and debug log above.
            Err(_) => Err(SwarmError::RetrievalExhausted { address: *address }),
        }
    }
}

/// Race the `close` set for the chunk, and on race-exhaustion widen to the
/// farther `spill` ring, reusing one dispatch closure and its shared attempt
/// counter.
///
/// A close-race deadline is terminal: the wall clock is spent, so widening would
/// only overrun it. Only an all-failed or no-candidates close race spills, which
/// is the difficult-chunk case where a present close set could not serve the
/// chunk but a farther holder still might. Each phase carries its own
/// [`RaceBounds`], so the spill's metered fan-out is bounded independently of the
/// close race rather than double-counting a shared budget.
async fn race_close_then_spill<C, T, E, F, Fut>(
    close: impl IntoIterator<Item = C>,
    spill: impl IntoIterator<Item = C>,
    close_bounds: RaceBounds,
    spill_bounds: RaceBounds,
    mut attempt: F,
) -> Result<T, RaceFailure<E>>
where
    F: FnMut(C) -> Option<Fut>,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    let close_outcome = race_with_refill(
        close,
        close_bounds.budget,
        close_bounds.max_in_flight,
        close_bounds.deadline,
        close_bounds.stagger,
        &mut attempt,
    )
    .await;
    match close_outcome {
        Ok(value) => Ok(value),
        Err(RaceFailure::TimedOut) => Err(RaceFailure::TimedOut),
        Err(RaceFailure::AllFailed(_) | RaceFailure::NoCandidates) => {
            race_with_refill(
                spill,
                spill_bounds.budget,
                spill_bounds.max_in_flight,
                spill_bounds.deadline,
                spill_bounds.stagger,
                &mut attempt,
            )
            .await
        }
    }
}

/// Build the bin-bucket proximity-routed candidate order for `chunk`.
///
/// Routes to the Kademlia forwarding bin `b = PO(local, chunk)` first: a peer
/// there shares the chunk's first `b` bits, so it is at least as close to the
/// chunk as we are and forwards over fewer hops. When that bin yields too few
/// peers the route spills outward to the adjacent bins (`b-1`, `b+1`, `b-2`,
/// ...) up to `width` candidates, so a sparse forwarding bin still finds an
/// entry point. Within every bin the peers are ordered closest-to-chunk first.
/// `peers_in_bin` returns connected peers only; retrieval never dials.
pub(crate) fn bin_routed_order(
    chunk: &SwarmAddress,
    local: &SwarmAddress,
    max_bin: u8,
    width: usize,
    peers_in_bin: impl Fn(Bin) -> Vec<SwarmAddress>,
) -> Vec<SwarmAddress> {
    let b = chunk.proximity(local).get().min(max_bin);
    let mut order: Vec<SwarmAddress> = Vec::with_capacity(width);
    for bin_index in spill_bins(b, max_bin) {
        if order.len() >= width {
            break;
        }
        let bin = Bin::new(bin_index).unwrap_or(Bin::MAX);
        let mut in_bin = peers_in_bin(bin);
        in_bin.sort_by_key(|peer| std::cmp::Reverse(chunk.proximity(peer)));
        order.extend(in_bin);
    }
    order.truncate(width);
    order
}

/// Bin visiting order for the bin-bucket route: the forwarding bin `b`, then
/// outward to the nearest bins on either side (`b-1`, `b+1`, `b-2`, `b+2`,
/// ...) within `[0, max_bin]`.
pub(crate) fn spill_bins(b: u8, max_bin: u8) -> Vec<u8> {
    let mut bins = Vec::with_capacity(max_bin as usize + 1);
    bins.push(b);
    let mut delta = 1u8;
    loop {
        let mut pushed = false;
        if let Some(lower) = b.checked_sub(delta) {
            bins.push(lower);
            pushed = true;
        }
        let upper = b.saturating_add(delta);
        if upper > b && upper <= max_bin {
            bins.push(upper);
            pushed = true;
        }
        if !pushed {
            break;
        }
        delta += 1;
    }
    bins
}

/// Record the metered attempts a successful retrieval spent beyond the one that
/// won.
///
/// Each extra attempt is a failed refill or a concurrent loser the race dropped;
/// a dropped loser is not cancelled downstream, so it still fetches and meters a
/// duplicate. This is the dispatch-side over-fetch signal (an upper bound). The
/// delivery-side count of duplicates that actually arrived is
/// `swarm.client.retrieval_overfetch_delivered`, emitted by the handler.
fn record_overfetch(attempts: usize, path: &'static str) {
    if let Some(extra) = attempts.checked_sub(1).filter(|extra| *extra > 0) {
        counter!("swarm.client.retrieval_overfetch_total", "path" => path).increment(extra as u64);
    }
}

/// Decide whether an origin uploader accepts a custody receipt from `peer`.
///
/// The receipt is a [`Receipt`]: its storer was recovered and verified at the
/// decode boundary (a malformed receipt never reaches here). This checks the
/// custody depth against the locally observed neighbourhood depth,
/// trust-but-verified against the receipt's own declared radius, gated on that
/// depth being credible (`neighbourhood_credible`).
///
/// The verdict drives the caller:
/// - [`DepthVerdict::Verified`]: the receipt is trusted; the push succeeded.
/// - [`DepthVerdict::Shallow`]: the storer is provably too shallow. The
///   responding peer is scored adversely for invalid data through the supplied
///   reporter, and the caller retries via a different route instead of
///   believing a fabricated shallow receipt.
/// - [`DepthVerdict::Unverifiable`]: the local view is not credible enough to
///   judge custody depth. The peer is NOT penalised (it may be honest); the
///   caller treats the push as unconfirmed.
fn accept_origin_receipt(
    receipt: &Receipt,
    peer: SwarmAddress,
    local_depth: NeighborhoodDepth,
    neighbourhood_credible: bool,
    reporter: &dyn PeerReporter,
) -> DepthVerdict {
    let verdict = receipt.verify_depth(local_depth, neighbourhood_credible);
    if let DepthVerdict::Shallow(err) = &verdict {
        warn!(
            %peer,
            address = %receipt.address,
            error = <&'static str>::from(err),
            "rejected shallow custody receipt; retrying another route"
        );
        reporter.report_peer(&peer, SwarmScoringEvent::InvalidData, PUSHSYNC_SOURCE);
    }
    verdict
}

#[cfg(test)]
mod tests {
    mod push_verdict {
        use std::sync::Mutex;

        use alloy_signer::SignerSync;
        use alloy_signer_local::PrivateKeySigner;
        use nectar_primitives::{NetworkId, Nonce, compute_overlay};
        use vertex_swarm_api::StorageRadius;
        use vertex_swarm_net_pushsync::WireReceipt;

        use super::super::*;

        const NET: NetworkId = NetworkId::MAINNET;

        #[derive(Default)]
        struct RecordingReporter {
            reports: Mutex<Vec<(SwarmAddress, SwarmScoringEvent, ReportSource)>>,
        }

        impl PeerReporter for RecordingReporter {
            fn report_peer(
                &self,
                overlay: &SwarmAddress,
                event: SwarmScoringEvent,
                source: ReportSource,
            ) {
                self.reports.lock().unwrap().push((*overlay, event, source));
            }
        }

        impl RecordingReporter {
            /// Return the single recorded report, asserting exactly one exists.
            fn single(&self) -> (SwarmAddress, SwarmScoringEvent, ReportSource) {
                let reports = self.reports.lock().unwrap();
                assert_eq!(reports.len(), 1, "expected exactly one report");
                *reports.first().expect("one report")
            }

            fn count(&self) -> usize {
                self.reports.lock().unwrap().len()
            }
        }

        fn address(first_byte: u8) -> ChunkAddress {
            let mut bytes = [0u8; 32];
            bytes[0] = first_byte;
            ChunkAddress::new(bytes)
        }

        /// A storer-verified receipt as the decode boundary produces it, with the
        /// storer ground to sit exactly `proximity` bits deep relative to
        /// `address`.
        ///
        /// The grind targets an exact proximity, not a lower bound: the depth
        /// verdict turns on the observed proximity, so a lower bound would leave
        /// it dependent on the random overlay and flake (a shallow case
        /// occasionally grinding deep enough to verify). An exact target makes
        /// every verdict deterministic.
        fn signed_receipt(
            signer: &PrivateKeySigner,
            address: &ChunkAddress,
            proximity: u8,
            storage_radius: StorageRadius,
        ) -> Receipt {
            let eth = signer.address();
            // The signature is over the 32-byte address only (the wire format)
            // and is independent of the nonce, so sign once and grind for overlay
            // depth.
            let signature = signer.sign_message_sync(address.as_bytes()).expect("sign");
            let mut counter = 0u64;
            loop {
                let mut nonce_bytes = [0u8; 32];
                nonce_bytes[..8].copy_from_slice(&counter.to_le_bytes());
                let nonce = Nonce::from(nonce_bytes);
                let overlay = compute_overlay(&eth, NET, &nonce);
                if address.proximity(&overlay).get() == proximity {
                    let wire = WireReceipt::new(*address, signature, nonce, storage_radius);
                    return Receipt::reconstruct(wire, NET).expect("reconstructs");
                }
                counter += 1;
            }
        }

        fn depth(n: u8) -> NeighborhoodDepth {
            NeighborhoodDepth::new(Bin::new(n).unwrap())
        }

        fn radius(n: u8) -> StorageRadius {
            StorageRadius::new(Bin::new(n).unwrap())
        }

        #[test]
        fn origin_accepts_a_deep_receipt_without_reporting() {
            let signer = PrivateKeySigner::random();
            let addr = address(0xff);
            let receipt = signed_receipt(&signer, &addr, 8, radius(8));
            let reporter = RecordingReporter::default();
            let peer = SwarmAddress::from([0x11; 32]);

            assert_eq!(
                accept_origin_receipt(&receipt, peer, depth(8), true, &reporter),
                DepthVerdict::Verified,
                "deep receipt accepted"
            );
            assert!(reporter.reports.lock().unwrap().is_empty());
        }

        #[test]
        fn origin_rejects_a_shallow_receipt_and_reports_the_peer() {
            let signer = PrivateKeySigner::random();
            let addr = address(0xff);
            // Shallow signer; against a credible local view the floor (depth 12)
            // rejects it regardless of the claimed radius.
            let receipt = signed_receipt(&signer, &addr, 0, radius(8));
            let reporter = RecordingReporter::default();
            let peer = SwarmAddress::from([0x22; 32]);

            let DepthVerdict::Shallow(_) =
                accept_origin_receipt(&receipt, peer, depth(12), true, &reporter)
            else {
                panic!("shallow receipt rejected");
            };

            let (reported_peer, event, source) = reporter.single();
            assert_eq!(reported_peer, peer, "the responding peer is scored");
            assert_eq!(event, SwarmScoringEvent::InvalidData);
            assert_eq!(source, ReportSource::Protocol("pushsync"));
        }

        #[test]
        fn origin_rejects_a_shallow_receipt_claiming_radius_zero() {
            // Regression: against a credible local view an attacker setting
            // storage_radius == 0 must not bypass the local floor at the origin
            // uploader.
            let signer = PrivateKeySigner::random();
            let addr = address(0xff);
            let receipt = signed_receipt(&signer, &addr, 0, radius(0));
            let reporter = RecordingReporter::default();
            let peer = SwarmAddress::from([0x55; 32]);

            assert!(
                matches!(
                    accept_origin_receipt(&receipt, peer, depth(12), true, &reporter),
                    DepthVerdict::Shallow(_)
                ),
                "radius 0 does not bypass the local floor"
            );
            assert_eq!(reporter.count(), 1);
        }

        #[test]
        fn origin_treats_an_unverifiable_receipt_as_unconfirmed_without_reporting() {
            // Regression for a non-credible local view (a fresh or sparse node,
            // local_depth == 0): a shallow receipt declaring radius 0 must NOT be
            // accepted, and the responder must NOT be penalised: the verdict is
            // unverifiable, not a finding of misbehaviour.
            let signer = PrivateKeySigner::random();
            let addr = address(0xff);
            let receipt = signed_receipt(&signer, &addr, 0, radius(0));
            let reporter = RecordingReporter::default();
            let peer = SwarmAddress::from([0x66; 32]);

            assert_eq!(
                accept_origin_receipt(&receipt, peer, depth(0), false, &reporter),
                DepthVerdict::Unverifiable,
                "non-credible view yields an unverifiable verdict"
            );
            assert_eq!(
                reporter.count(),
                0,
                "an unverifiable receipt does not penalise the peer"
            );
        }
    }

    mod bin_route {
        use std::collections::HashMap;

        use super::super::{bin_routed_order, spill_bins};
        use nectar_primitives::SwarmAddress;
        use vertex_swarm_api::Bin;

        /// An address with `byte0` set, the rest zero, so its proximity to the
        /// zero local overlay is controllable.
        fn addr(byte0: u8) -> SwarmAddress {
            let mut bytes = [0u8; 32];
            bytes[0] = byte0;
            SwarmAddress::from(bytes)
        }

        #[test]
        fn spill_visits_the_forwarding_bin_then_outward() {
            // Mid bin: b, then b-1, b+1, b-2, b+2, ... clamped to [0, max].
            assert_eq!(spill_bins(4, 8), vec![4, 3, 5, 2, 6, 1, 7, 0, 8]);
        }

        #[test]
        fn spill_clamps_at_both_edges() {
            // b == 0 never produces a negative bin; b == max never overshoots.
            assert_eq!(spill_bins(0, 3), vec![0, 1, 2, 3]);
            assert_eq!(spill_bins(3, 3), vec![3, 2, 1, 0]);
            assert_eq!(spill_bins(0, 0), vec![0]);
        }

        #[test]
        fn routes_to_the_forwarding_bin_first_then_spills() {
            // local is the zero overlay; chunk 0x08 (0000_1000) shares its first
            // four bits with local, so the forwarding bin is 4.
            let local = addr(0x00);
            let chunk = addr(0x08);
            // One sentinel peer per bin, encoding the bin index in byte 1.
            let peers = |bin: Bin| {
                let mut bytes = [0u8; 32];
                bytes[1] = bin.get();
                bytes[2] = 0x01; // distinguish from local/chunk
                vec![SwarmAddress::from(bytes)]
            };

            let order = bin_routed_order(&chunk, &local, 8, 32, peers);
            let bins: Vec<u8> = order
                .iter()
                .map(|p| p.as_bytes().get(1).copied().unwrap())
                .collect();
            assert_eq!(
                bins,
                vec![4, 3, 5, 2, 6, 1, 7, 0, 8],
                "the forwarding bin leads, then the route spills outward"
            );
        }

        #[test]
        fn orders_within_a_bin_by_closeness_to_the_chunk() {
            let local = addr(0x00);
            let chunk = addr(0x08);
            let near = addr(0x08); // shares the chunk's prefix: high proximity
            let far = addr(0xff); // diverges at the first bit: proximity 0
            let b = chunk.proximity(&local).get();
            let bin_b = b;
            let peers = move |bin: Bin| {
                if bin.get() == bin_b {
                    vec![far, near]
                } else {
                    Vec::new()
                }
            };

            let order = bin_routed_order(&chunk, &local, 31, 32, peers);
            assert_eq!(
                order,
                vec![near, far],
                "the closer-to-chunk peer leads its bin regardless of input order"
            );
        }

        #[test]
        fn respects_the_width_cap() {
            let local = addr(0x00);
            let chunk = addr(0x08);
            // Three peers in every bin; a width of 5 takes only the first five.
            let peers = |bin: Bin| {
                (0u8..3)
                    .map(|i| {
                        let mut bytes = [0u8; 32];
                        bytes[1] = bin.get();
                        bytes[2] = i + 1;
                        SwarmAddress::from(bytes)
                    })
                    .collect::<Vec<_>>()
            };
            let order = bin_routed_order(&chunk, &local, 8, 5, peers);
            assert_eq!(order.len(), 5, "width caps the candidate count");
        }

        #[test]
        fn empty_bins_yield_no_candidates() {
            let local = addr(0x00);
            let chunk = addr(0x08);
            let empty: HashMap<u8, Vec<SwarmAddress>> = HashMap::new();
            let order = bin_routed_order(&chunk, &local, 8, 32, |bin| {
                empty.get(&bin.get()).cloned().unwrap_or_default()
            });
            assert!(order.is_empty(), "no connected peers yields no route");
        }
    }

    mod null_objects {
        use nectar_primitives::SwarmAddress;
        use vertex_swarm_api::ChunkAddress;
        use vertex_tasks::time::Duration;

        use super::super::{CandidateOrdering, LatencyHint, NoLatencyHint, ProximityOnly};

        fn overlay(n: u8) -> SwarmAddress {
            SwarmAddress::from([n; 32])
        }

        #[test]
        fn proximity_only_leaves_the_order_untouched() {
            let candidates = vec![overlay(3), overlay(1), overlay(2)];
            let chunk = ChunkAddress::zero();
            assert_eq!(
                ProximityOnly.order(candidates.clone(), &chunk),
                candidates,
                "ordering is a no-op"
            );
            assert_eq!(
                ProximityOnly.order_closest_admissible(candidates.clone(), &chunk),
                candidates,
                "the spill ordering is a no-op"
            );
        }

        #[test]
        fn no_latency_hint_is_always_cold() {
            assert_eq!(
                NoLatencyHint.estimate(8),
                None::<Duration>,
                "no estimate, so the stagger falls back to the constant"
            );
        }
    }

    mod inflight_available {
        use std::num::NonZeroUsize;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use nectar_primitives::SwarmAddress;

        use super::super::{
            InflightLimit, RETRIEVE_ATTEMPT_BUDGET, RETRIEVE_DEADLINE, RETRIEVE_MAX_IN_FLIGHT,
            RETRIEVE_SPILL_BUDGET, RETRIEVE_SPILL_DEADLINE, RaceBounds, race_close_then_spill,
        };
        use crate::{PeerInflightLimiter, RETRIEVAL_STAGGER, RaceFailure};

        const CAP_ONE: NonZeroUsize = match NonZeroUsize::new(1) {
            Some(cap) => cap,
            None => unreachable!(),
        };

        fn overlay(n: u8) -> SwarmAddress {
            SwarmAddress::from([n; 32])
        }

        #[test]
        fn available_surfaces_a_busy_peer_as_a_tail() {
            let limiter = PeerInflightLimiter::new(CAP_ONE);
            let busy = overlay(1);
            let _held = limiter.try_acquire(&busy).expect("first slot");

            let (ordered, enforce_cap) = limiter.available(vec![busy, overlay(2), overlay(3)]);
            assert_eq!(
                ordered,
                vec![overlay(2), overlay(3), busy],
                "the free peers lead, the busy peer is appended as a tail, never dropped"
            );
            assert!(
                !enforce_cap,
                "free-first ordering keeps the tail a last resort, so the cap is not enforced"
            );
        }

        #[test]
        fn available_returns_the_full_list_when_every_candidate_is_capped() {
            let limiter = PeerInflightLimiter::new(CAP_ONE);
            let candidates = vec![overlay(1), overlay(2)];
            let _h1 = limiter.try_acquire(&overlay(1)).expect("slot a");
            let _h2 = limiter.try_acquire(&overlay(2)).expect("slot b");

            let (ordered, enforce_cap) = limiter.available(candidates.clone());
            assert_eq!(
                ordered, candidates,
                "an all-busy set keeps every peer rather than failing"
            );
            assert!(
                !enforce_cap,
                "the all-busy list is best-effort, not cap-enforced"
            );
        }

        /// A present-but-failing close set spills to a farther holder that is busy
        /// at its in-flight cap: `available` surfaces the busy holder as a tail
        /// (Part A) and the spill race reaches it best-effort after the close race
        /// exhausts (Part B), so the difficult chunk is served rather than failing.
        #[tokio::test]
        async fn failing_close_set_spills_to_a_busy_farther_holder() {
            let limiter = PeerInflightLimiter::new(CAP_ONE);
            let failing_close = overlay(1);
            let holder = overlay(2);
            // The farther holder is at its cap when the race begins.
            let _held = limiter.try_acquire(&holder).expect("saturate the holder");

            // The busy holder is surfaced (never dropped), as a tail, cap off.
            let (spill, enforce_cap) = limiter.available(vec![holder]);
            assert_eq!(
                spill,
                vec![holder],
                "the busy holder is surfaced, not dropped"
            );
            assert!(!enforce_cap, "the busy tail is best-effort, the cap is off");

            let attempts = Arc::new(AtomicUsize::new(0));
            let counted = Arc::clone(&attempts);
            let limiter_ref = &limiter;
            let outcome = race_close_then_spill(
                vec![failing_close],
                spill,
                RaceBounds {
                    budget: RETRIEVE_ATTEMPT_BUDGET,
                    max_in_flight: RETRIEVE_MAX_IN_FLIGHT,
                    deadline: RETRIEVE_DEADLINE,
                    stagger: RETRIEVAL_STAGGER,
                },
                RaceBounds {
                    budget: RETRIEVE_SPILL_BUDGET,
                    max_in_flight: RETRIEVE_MAX_IN_FLIGHT,
                    deadline: RETRIEVE_SPILL_DEADLINE,
                    stagger: RETRIEVAL_STAGGER,
                },
                |peer: SwarmAddress| {
                    counted.fetch_add(1, Ordering::SeqCst);
                    // Best-effort dispatch: the busy holder has no free permit, but
                    // the enforce-off race still contacts it.
                    let _permit = limiter_ref.try_acquire(&peer);
                    Some(async move {
                        if peer == holder {
                            Ok(holder)
                        } else {
                            Err("close entry point could not serve it")
                        }
                    })
                },
            )
            .await;

            assert_eq!(
                outcome.ok(),
                Some(holder),
                "the spill reaches the busy farther holder the close set never held"
            );
        }

        /// A close race that only times out does not spill: the wall clock is
        /// spent, so widening would overrun it.
        #[tokio::test]
        async fn a_close_deadline_is_terminal_and_does_not_spill() {
            use futures_timer::Delay;
            use std::time::Duration;

            let spill_dispatched = Arc::new(AtomicUsize::new(0));
            let counted = Arc::clone(&spill_dispatched);
            let outcome: Result<u32, RaceFailure<&str>> = race_close_then_spill(
                vec![0u32],
                vec![1u32],
                RaceBounds {
                    budget: 4,
                    max_in_flight: RETRIEVE_MAX_IN_FLIGHT,
                    deadline: Duration::from_millis(100),
                    stagger: Duration::from_secs(3600),
                },
                RaceBounds {
                    budget: 4,
                    max_in_flight: RETRIEVE_MAX_IN_FLIGHT,
                    deadline: Duration::from_secs(1),
                    stagger: RETRIEVAL_STAGGER,
                },
                |candidate: u32| {
                    if candidate == 1 {
                        counted.fetch_add(1, Ordering::SeqCst);
                    }
                    Some(async move {
                        // The close candidate withholds past its deadline; the spill
                        // candidate would serve if ever reached.
                        if candidate == 0 {
                            Delay::new(Duration::from_secs(30)).await;
                        }
                        Ok::<u32, &str>(candidate)
                    })
                },
            )
            .await;

            assert!(
                matches!(outcome, Err(RaceFailure::TimedOut)),
                "a timed-out close race is terminal"
            );
            assert_eq!(
                spill_dispatched.load(Ordering::SeqCst),
                0,
                "the spill phase never ran after a close deadline"
            );
        }
    }

    /// The fully-gated settle-drive: when the band refuses every candidate, the
    /// fallback must drive a settle on the closest gated peers and re-select,
    /// bounded, rather than exhaust having contacted nobody. Testable here only
    /// because the engine is generic over [`RetrievalTopology`], so a mock stands
    /// in for the real handle.
    mod settle_drive {
        use std::num::NonZeroUsize;
        use std::sync::{Arc, Mutex};

        use vertex_swarm_api::{Bin, ChunkAddress, OverlayAddress, SwarmError};
        use vertex_swarm_test_utils::MockTopology;

        use super::super::{
            CandidateOrdering, DispatchEngine, NoLatencyHint, RETRIEVE_SETTLE_DRIVES,
            RetrievalTopology,
        };
        use crate::ClientHandle;
        use crate::inflight::PeerInflightLimiter;
        use crate::selection::SettlementTrigger;

        fn overlay(byte: u8) -> OverlayAddress {
            OverlayAddress::from([byte; 32])
        }

        /// Ordering that gates every candidate: the fully-refused band.
        #[derive(Clone)]
        struct GateAll;
        impl CandidateOrdering for GateAll {
            fn order(&self, _: Vec<OverlayAddress>, _: &ChunkAddress) -> Vec<OverlayAddress> {
                Vec::new()
            }
            fn order_closest_admissible(
                &self,
                _: Vec<OverlayAddress>,
                _: &ChunkAddress,
            ) -> Vec<OverlayAddress> {
                Vec::new()
            }
        }

        /// Records every peer the settle-drive fires on.
        #[derive(Clone, Default)]
        struct RecordingSettle {
            calls: Arc<Mutex<Vec<OverlayAddress>>>,
        }
        impl SettlementTrigger for RecordingSettle {
            fn trigger_settlement(&self, peer: OverlayAddress) {
                self.calls.lock().unwrap().push(peer);
            }
        }

        #[tokio::test]
        async fn a_fully_gated_fallback_drives_settles_then_exhausts() {
            // Every candidate is banded out, so the fallback finds no admissible
            // close or spill peer. It must drive a settle on the gated set each
            // round, bounded by the drive budget, then surface the honest
            // RetrievalExhausted rather than exhaust having contacted nobody.
            let peers: Vec<OverlayAddress> = (1..=4).map(overlay).collect();
            // The mock's `closest_to` returns this set (its `connected_peers_in_bin`
            // is empty, so the primary bin route yields nothing and the fallback
            // drives the settle).
            let topology: Arc<dyn RetrievalTopology> =
                Arc::new(MockTopology::new(4, 4, 0).with_closest(peers.clone()));
            let settle = RecordingSettle::default();
            let (tx, _rx) = tokio::sync::mpsc::channel(16);
            let engine = DispatchEngine::new(
                ClientHandle::new(tx),
                topology,
                Bin::MAX,
                GateAll,
                PeerInflightLimiter::new(NonZeroUsize::new(4).unwrap()),
                NoLatencyHint,
                Arc::new(settle.clone()),
            );

            let result = engine.retrieve(&ChunkAddress::from([0x42; 32])).await;

            assert!(
                matches!(result, Err(SwarmError::RetrievalExhausted { .. })),
                "a fully-gated retrieval exhausts, never claims absence"
            );
            let calls = settle.calls.lock().unwrap();
            assert_eq!(
                calls.len(),
                RETRIEVE_SETTLE_DRIVES * peers.len(),
                "each of the bounded drive rounds settles the full gated set"
            );
        }
    }
}
