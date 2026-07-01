//! Shared dispatch engine for origin chunk retrieval.
//!
//! Both the native and browser chunk providers build a [`RetrievalEngine`] and
//! delegate their `retrieve_chunk` implementation to it, so the bin-route
//! primary, staggered refilling race, in-flight cap, adaptive stagger, headroom
//! spill, and over-fetch metrics live in one place. Each capability is a small
//! trait the engine is generic over: the native client supplies
//! [`PeerSelector`], [`PeerInflightLimiter`], and [`RetrievalLatency`]; the
//! browser supplies the zero-sized null objects [`ProximityOnly`],
//! [`NoInflightLimit`], and [`NoLatencyHint`].

use std::sync::atomic::{AtomicUsize, Ordering};

use metrics::{counter, histogram};
use nectar_primitives::SwarmAddress;
use tokio::sync::OwnedSemaphorePermit;
use vertex_swarm_api::{
    Bin, ChunkAddress, ChunkRetrievalResult, OverlayAddress, SwarmError, SwarmIdentity,
    SwarmResult, SwarmTopologyPeers, SwarmTopologyRouting, SwarmTopologyState,
};
use vertex_swarm_topology::TopologyHandle;
use vertex_tasks::time::Duration;

use crate::retrieval_latency::{RetrievalLatency, adaptive_stagger};
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

/// Stagger for the primary route, set beyond [`PRIMARY_ROUTE_DEADLINE`] so it
/// never fires: the primary is strict single-flight.
///
/// Exactly one metered attempt is in flight at a time on the primary path. The
/// staggered fan-out is reserved for the fallback.
const PRIMARY_ROUTE_SINGLE_FLIGHT: Duration = Duration::from_secs(3600);

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
/// The native client supplies [`PeerInflightLimiter`]; the browser supplies
/// [`NoInflightLimit`], which never declines a peer.
#[auto_impl::auto_impl(&, Arc)]
pub trait InflightLimit: Send + Sync {
    /// Per-attempt reservation that releases the slot on drop, including a
    /// cancelled losing attempt.
    type Permit: vertex_tasks::MaybeSend + 'static;

    /// Filter proximity-ordered `candidates` to those with a free slot,
    /// returning the survivors and whether the race should enforce the cap. When
    /// every candidate is at its cap the full list is returned with the cap not
    /// enforced (degraded service beats failing the request).
    fn available(&self, candidates: Vec<OverlayAddress>) -> (Vec<OverlayAddress>, bool);

    /// Reserve a slot for `peer`, or `None` when it is at its cap.
    fn try_acquire(&self, peer: &OverlayAddress) -> Option<Self::Permit>;
}

impl InflightLimit for PeerInflightLimiter {
    type Permit = OwnedSemaphorePermit;

    fn available(&self, candidates: Vec<OverlayAddress>) -> (Vec<OverlayAddress>, bool) {
        let survivors: Vec<OverlayAddress> = candidates
            .iter()
            .copied()
            .filter(|peer| self.has_free_slot(peer))
            .collect();
        // The cap is enforced only when free-slot peers were found: the all-busy
        // fall-through returns the full list so the race still attempts,
        // best-effort.
        if survivors.is_empty() {
            (candidates, false)
        } else {
            (survivors, true)
        }
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

/// No per-peer cap: every candidate has a free slot. The browser client's
/// [`InflightLimit`].
#[derive(Clone, Copy, Debug, Default)]
pub struct NoInflightLimit;

impl InflightLimit for NoInflightLimit {
    type Permit = ();

    fn available(&self, candidates: Vec<OverlayAddress>) -> (Vec<OverlayAddress>, bool) {
        (candidates, false)
    }

    fn try_acquire(&self, _peer: &OverlayAddress) -> Option<Self::Permit> {
        Some(())
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

/// Tuning for one staggered race phase: the lifetime attempt `budget`, the
/// concurrent-attempt width `max_in_flight`, the wall-clock `deadline`, and the
/// per-attempt `stagger`.
struct RaceBounds {
    budget: usize,
    max_in_flight: usize,
    deadline: Duration,
    stagger: Duration,
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
pub struct RetrievalEngine<I: SwarmIdentity, O: CandidateOrdering, G: InflightLimit, L: LatencyHint>
{
    client_handle: ClientHandle,
    topology: TopologyHandle<I>,
    ordering: O,
    inflight: G,
    latency: L,
}

impl<I, O, G, L> RetrievalEngine<I, O, G, L>
where
    I: SwarmIdentity,
    O: CandidateOrdering,
    G: InflightLimit,
    L: LatencyHint,
{
    /// Build an engine over its three capabilities.
    pub fn new(
        client_handle: ClientHandle,
        topology: TopologyHandle<I>,
        ordering: O,
        inflight: G,
        latency: L,
    ) -> Self {
        Self {
            client_handle,
            topology,
            ordering,
            inflight,
            latency,
        }
    }

    /// The topology handle, for the native push path's closest-storer dispatch;
    /// retrieval reaches topology through the engine's own dispatch.
    pub(crate) fn topology(&self) -> &TopologyHandle<I> {
        &self.topology
    }

    /// The client handle, for dispatching a push on the native push path.
    pub(crate) fn client_handle(&self) -> &ClientHandle {
        &self.client_handle
    }

    /// Order `candidates` through the accounting band, for the native push path.
    ///
    /// Drops refused peers and ranks by score; with [`ProximityOnly`] the order
    /// is unchanged.
    pub(crate) fn order(
        &self,
        candidates: Vec<OverlayAddress>,
        chunk: &ChunkAddress,
    ) -> Vec<OverlayAddress> {
        self.ordering.order(candidates, chunk)
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
        let max_bin = self.topology.max_bin().get();
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
                    RaceBounds {
                        budget: PRIMARY_ROUTE_BUDGET,
                        // Single-flight: at most one metered attempt in flight,
                        // advancing the bin spill only on an explicit failure.
                        max_in_flight: 1,
                        deadline: PRIMARY_ROUTE_DEADLINE,
                        stagger: PRIMARY_ROUTE_SINGLE_FLIGHT,
                    },
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
        let closest_peers = self.topology.closest_to(&chunk_address, RETRIEVE_WIDTH);
        // Spill to a wider in-headroom slice when every close peer is gated, so a
        // fully gated close set routes around its spent peers rather than
        // blocking; if even the wide slice is gated the empty result falls through
        // to the no-connected-peers path below, the same RetrievalExhausted a
        // no-peers failure yields, and the consumer re-streams.
        let closest_peers = self.order_with_spill(closest_peers, address);
        let (candidates, enforce_cap) = self.inflight.available(closest_peers);

        // Pace the staggered fan-out to the live round trip rather than a fixed
        // constant. Each candidate's expected latency is read from the per-PO
        // estimate at its proximity to the chunk (the forwarding distance that
        // dominates retrieval latency). Cold buckets fall back to the constant, so
        // this is never slower, only faster on low-RTT distances.
        let stagger = adaptive_stagger(
            candidates
                .iter()
                .map(|peer| self.latency.estimate(chunk_address.proximity(peer).get())),
        );

        let outcome = self
            .race_attempts(
                candidates,
                chunk_address,
                RaceBounds {
                    budget: RETRIEVE_ATTEMPT_BUDGET,
                    max_in_flight: RETRIEVE_MAX_IN_FLIGHT,
                    deadline: RETRIEVE_DEADLINE,
                    stagger,
                },
                enforce_cap,
                &attempts,
            )
            .await;

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

#[cfg(test)]
mod tests {
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

        use super::super::{
            CandidateOrdering, InflightLimit, LatencyHint, NoInflightLimit, NoLatencyHint,
            ProximityOnly,
        };

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
        fn no_inflight_limit_keeps_every_candidate_and_never_declines() {
            let candidates = vec![overlay(1), overlay(2)];
            assert_eq!(
                NoInflightLimit.available(candidates.clone()),
                (candidates, false),
                "no peer is filtered and the cap is not enforced"
            );
            assert_eq!(
                NoInflightLimit.try_acquire(&overlay(1)),
                Some(()),
                "a permit is always granted"
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

        use nectar_primitives::SwarmAddress;

        use super::super::InflightLimit;
        use crate::PeerInflightLimiter;

        const CAP_ONE: NonZeroUsize = match NonZeroUsize::new(1) {
            Some(cap) => cap,
            None => unreachable!(),
        };

        fn overlay(n: u8) -> SwarmAddress {
            SwarmAddress::from([n; 32])
        }

        #[test]
        fn available_drops_a_capped_head() {
            let limiter = PeerInflightLimiter::new(CAP_ONE);
            let busy = overlay(1);
            let _held = limiter.try_acquire(&busy).expect("first slot");

            let (survivors, enforce_cap) = limiter.available(vec![busy, overlay(2), overlay(3)]);
            assert_eq!(
                survivors,
                vec![overlay(2), overlay(3)],
                "the capped head is skipped, the next-closest free peers remain"
            );
            assert!(enforce_cap, "free-slot peers found, so the cap is enforced");
        }

        #[test]
        fn available_falls_through_when_every_candidate_is_capped() {
            let limiter = PeerInflightLimiter::new(CAP_ONE);
            let candidates = vec![overlay(1), overlay(2)];
            let _h1 = limiter.try_acquire(&overlay(1)).expect("slot a");
            let _h2 = limiter.try_acquire(&overlay(2)).expect("slot b");

            let (survivors, enforce_cap) = limiter.available(candidates.clone());
            assert_eq!(
                survivors, candidates,
                "all-busy falls through to the full list rather than failing"
            );
            assert!(
                !enforce_cap,
                "the all-busy fall-through is best-effort, not cap-enforced"
            );
        }
    }
}
