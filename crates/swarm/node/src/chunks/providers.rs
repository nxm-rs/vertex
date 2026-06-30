//! RPC provider implementations for Swarm nodes.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use metrics::{counter, histogram};
use nectar_primitives::SwarmAddress;
use tracing::warn;
use vertex_swarm_api::{
    Bin, ChunkAddress, ChunkRetrievalResult, PeerReporter, PushReceipt, ReportSource, StampedChunk,
    SwarmChunkProvider, SwarmChunkSender, SwarmError, SwarmIdentity, SwarmResult,
    SwarmScoringEvent, SwarmTopologyPeers, SwarmTopologyRouting, SwarmTopologyState,
};
use vertex_swarm_net_pushsync::{DepthVerdict, Receipt};
use vertex_swarm_topology::TopologyHandle;
// `Instant` is portable (the browser performance clock on wasm); only timer
// sleeps are `!Send`, and the gated-set wait awaits a settle completion instead.
use vertex_tasks::time::{Duration, Instant};

use crate::{
    ChunkTransferError, ClientHandle, PeerInflightLimiter, PeerSelector, RETRIEVAL_STAGGER,
    RaceFailure, RetrievalResult, race_walk,
};

/// Report source for shallow/malformed receipts caught on the origin upload
/// path.
const PUSHSYNC_SOURCE: ReportSource = ReportSource::Protocol("pushsync");

/// Number of closest peers to try when pushing a chunk before giving up.
const PUSH_CANDIDATE_COUNT: usize = 5;

/// Proximity-ordered pool of closest connected peers the retrieval walk draws
/// from.
///
/// Wider than [`RETRIEVE_LEG_BUDGET`] so that, after skip-busy removes peers at
/// their in-flight cap, the walk still has next-closest free-slot entry points to
/// refill a failed leg with rather than overrunning a hot peer. Retrieval is
/// forwarding-based, so these are connected peers only; the walk never dials.
const RETRIEVE_WALK_WIDTH: usize = 32;

/// Maximum real retrieval legs the walk dispatches per chunk before giving up.
///
/// A chunk whose closest few entry points miss is not absent: each leg is a
/// distinct peer whose own forwarding chain may still reach the chunk's storers,
/// so a sparse-bin chunk needs more than the closest few tries. Peers skipped for
/// back-pressure before dispatch do not count against this budget, so it bounds
/// only genuine coverage attempts and the metered bandwidth they cost. The
/// reference node's origin ceiling is 32; this starts conservative and is raised
/// only on the residual-failure metric below.
const RETRIEVE_LEG_BUDGET: usize = 8;

/// Maximum legs the staggered fallback keeps concurrently in flight, bounding the
/// metered over-fetch independently of [`RETRIEVE_LEG_BUDGET`].
///
/// A losing race leg is not cancelled downstream: the serving chain forwards and
/// delivers regardless, so every overlapping leg is real duplicate bandwidth and
/// cost. The stagger grows the in-flight set one leg per tick, so a withhold-storm
/// would otherwise reach the full budget concurrently. This caps the simultaneous
/// legs at a handful while [`RETRIEVE_LEG_BUDGET`] still bounds the lifetime total;
/// a failed leg refills at once, replacing a freed slot rather than widening, so
/// the walk keeps its reach.
const RETRIEVE_WALK_MAX_IN_FLIGHT: usize = 3;

/// Wall-clock bound on the whole retrieval walk across its refilled legs, so a
/// run of slow or withholding entry points cannot hold a download-pipeline slot
/// indefinitely. Matches the per-request retrieval lifetime.
const RETRIEVE_WALK_DEADLINE: Duration = Duration::from_secs(30);

/// Bound on the settle-and-await for a fully gated close set: the request's own
/// retrieval lifetime, matching the client-behaviour outbound retrieval timeout.
/// Accounting-timing back-pressure blocks within the request rather than failing
/// early to the consumer, and only a genuine lifetime expiry falls through to the
/// generic transient failure. The wait is progress-aware, so it paces on
/// settlement RTT and returns at once when no settle is in flight to drain debt.
const GATE_SETTLE_BUDGET: Duration = Duration::from_secs(30);

/// Legs the bin-bucket primary route dispatches before handing off to the
/// staggered fallback.
///
/// The primary routes a chunk to its Kademlia forwarding bin and tries the
/// in-headroom connected peers there (then the adjacent bins) one at a time. A
/// peer already sharing the chunk's prefix forwards over fewer hops, so the
/// closest few are the high-probability entry points; past that the difficult
/// chunk is better served by the wider staggered fallback than by more
/// single-flight bin tries. Bounds the metered legs the primary spends.
const PRIMARY_ROUTE_BUDGET: usize = 4;

/// Wall-clock bound on the whole bin-bucket primary route.
///
/// The primary is single-flight, so a withholding head holds the one in-flight
/// leg with no overlapping leg to overtake it. This deadline hands a stuck route
/// off to the staggered fallback rather than stalling the chunk's pipeline slot;
/// the forwarding bin's peers are close, so a genuine answer arrives well inside
/// it and only a stuck route pays the full wait.
const PRIMARY_ROUTE_DEADLINE: Duration = Duration::from_secs(2);

/// Stagger for the primary route, set beyond [`PRIMARY_ROUTE_DEADLINE`] so it
/// never fires: the primary is strict single-flight.
///
/// Exactly one metered leg is in flight at a time, so the happy path delivers a
/// chunk with no concurrent second leg and over-fetches nothing. A leg advances
/// only on an explicit failure (the bin spill), and a withholding head falls
/// through to the staggered fallback at the deadline instead of being raced. The
/// staggered fan-out that trades a second metered leg for failover latency is
/// reserved for that fallback, never the primary.
const PRIMARY_ROUTE_SINGLE_FLIGHT: Duration = Duration::from_secs(3600);

/// Tuning for one staggered walk phase: the lifetime leg `budget`, the
/// concurrent-leg width `max_in_flight`, the wall-clock `deadline`, and the
/// per-leg `stagger`.
struct WalkBounds {
    budget: usize,
    max_in_flight: usize,
    deadline: Duration,
    stagger: Duration,
}

/// Chunk provider using ClientHandle for network retrieval.
#[derive(Clone)]
pub struct NetworkChunkProvider<I: SwarmIdentity> {
    client_handle: ClientHandle,
    topology: TopologyHandle<I>,
    selector: Option<Arc<PeerSelector>>,
    inflight: Option<Arc<PeerInflightLimiter>>,
}

impl<I: SwarmIdentity> NetworkChunkProvider<I> {
    pub fn new(client_handle: ClientHandle, topology: TopologyHandle<I>) -> Self {
        Self {
            client_handle,
            topology,
            selector: None,
            inflight: None,
        }
    }

    /// Order retrieval and pushsync candidates with `selector` (score- and
    /// affordability-aware) instead of plain proximity order.
    pub fn with_selector(mut self, selector: Arc<PeerSelector>) -> Self {
        self.selector = Some(selector);
        self
    }

    /// Cap concurrent outbound retrieval substreams per peer, skipping a peer at
    /// its cap in favour of the next-closest candidate with a free slot.
    pub fn with_inflight_limiter(mut self, inflight: Arc<PeerInflightLimiter>) -> Self {
        self.inflight = Some(inflight);
        self
    }

    /// Order proximity-sorted `candidates` for a request on `chunk`, settling
    /// and awaiting a fully gated close set.
    ///
    /// With a selector this delegates to the score- and affordability-aware
    /// ordering. A non-empty order returns at once (the fast path); a fully
    /// gated close set awaits its in-flight settles up to the retrieval-lifetime
    /// [`GATE_SETTLE_BUDGET`], returning at the deadline or promptly once no
    /// settle is in flight to drain debt, with whatever is admissible (possibly
    /// empty). An empty result falls through to the caller's generic transient
    /// failure, never an accounting-specific error. The wait is `Send` on every
    /// platform. With no selector the proximity order is returned unchanged.
    async fn select_or_wait(
        &self,
        candidates: Vec<SwarmAddress>,
        chunk: &ChunkAddress,
    ) -> Vec<SwarmAddress> {
        match &self.selector {
            Some(selector) => {
                let deadline = Instant::now() + GATE_SETTLE_BUDGET;
                selector.order_or_wait(candidates, chunk, deadline).await
            }
            None => candidates,
        }
    }

    /// Order `candidates` through the accounting band without awaiting a settle.
    ///
    /// Used by the bin-bucket primary, where a gated route must fall through to
    /// the staggered fallback (which does await) rather than block here. Drops
    /// refused peers and ranks by score; with no selector the order is unchanged.
    fn select_now(&self, candidates: Vec<SwarmAddress>, chunk: &ChunkAddress) -> Vec<SwarmAddress> {
        match &self.selector {
            Some(selector) => selector.order(candidates, chunk),
            None => candidates,
        }
    }

    /// Run one bounded staggered walk over `candidates`, dispatching each leg as
    /// an originated retrieval that reserves the per-peer in-flight permit riding
    /// its future. Shared by the single-flight primary route and the staggered
    /// fallback; `legs` accumulates the metered legs across both phases.
    ///
    /// With `enforce_cap`, a peer that filled its in-flight slot since the
    /// skip-busy snapshot is declined at dispatch (no leg, no budget unit) so the
    /// cap holds on live state; without it (no limiter, or the all-busy
    /// fall-through) the leg runs best-effort even with no permit.
    async fn walk_legs(
        &self,
        candidates: Vec<SwarmAddress>,
        chunk_address: SwarmAddress,
        bounds: WalkBounds,
        enforce_cap: bool,
        legs: &AtomicUsize,
    ) -> Result<RetrievalResult, RaceFailure<ChunkTransferError>> {
        race_walk(
            candidates,
            bounds.budget,
            bounds.max_in_flight,
            bounds.deadline,
            bounds.stagger,
            |peer_overlay| {
                let permit = self
                    .inflight
                    .as_ref()
                    .and_then(|limiter| limiter.try_acquire(&peer_overlay));
                // A peer that filled since the skip-busy snapshot is declined so
                // the cap holds on live state, spending no budget; best-effort
                // (no limiter or all-busy fall-through) attempts without a permit.
                if enforce_cap && permit.is_none() {
                    return None;
                }
                legs.fetch_add(1, Ordering::Relaxed);
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
}

#[async_trait]
impl<I: SwarmIdentity> SwarmChunkProvider for NetworkChunkProvider<I> {
    async fn retrieve_chunk(&self, address: &ChunkAddress) -> SwarmResult<ChunkRetrievalResult> {
        let chunk_address = SwarmAddress::new(address.0.into());
        let legs = AtomicUsize::new(0);

        // PRIMARY: bin-bucket proximity route. Route the chunk to its Kademlia
        // forwarding bin b = PO(local, chunk) and dispatch the best in-headroom
        // connected peer there, spilling to the adjacent bins on saturation. A
        // peer in bin b already shares the chunk's first b bits, so it forwards
        // over fewer hops than a peer picked by raw closeness alone: fewer hops
        // is lower per-chunk latency, the throughput lever. The route is
        // single-flight (no stagger), so the happy path delivers one chunk for
        // one metered leg and over-fetches nothing. The accounting band is
        // applied without awaiting a settle here; a gated route falls through to
        // the fallback, which does await.
        let local = self.topology.overlay_address();
        let max_bin = self.topology.max_bin().get();
        let bin_candidates = bin_routed_order(
            &chunk_address,
            &local,
            max_bin,
            RETRIEVE_WALK_WIDTH,
            |bin| self.topology.connected_peers_in_bin(bin),
        );
        let bin_candidates = self.select_now(bin_candidates, &chunk_address);
        // Skip-busy: drop peers at their in-flight cap so the route leads with an
        // in-headroom peer. The cap is the non-economic muxer guard, composed
        // after the accounting band, never merged with it.
        let (bin_candidates, enforce_cap) = skip_busy(bin_candidates, self.inflight.as_deref());

        if !bin_candidates.is_empty() {
            let primary = self
                .walk_legs(
                    bin_candidates,
                    chunk_address,
                    WalkBounds {
                        budget: PRIMARY_ROUTE_BUDGET,
                        // Single-flight: at most one metered leg in flight,
                        // advancing the bin spill only on an explicit failure.
                        max_in_flight: 1,
                        deadline: PRIMARY_ROUTE_DEADLINE,
                        stagger: PRIMARY_ROUTE_SINGLE_FLIGHT,
                    },
                    enforce_cap,
                    &legs,
                )
                .await;
            if let Ok(result) = primary {
                histogram!("retrieval_walk_legs").record(legs.load(Ordering::Relaxed) as f64);
                counter!("retrieval_walk_total", "outcome" => "hit", "path" => "bin_route")
                    .increment(1);
                return Ok(ChunkRetrievalResult {
                    chunk: result.chunk,
                    stamp: result.stamp,
                    served_by: result.peer,
                });
            }
        }

        // FALLBACK: the staggered bounded-refill walk over the globally closest
        // connected peers. Reached when the bin route is gated, saturated, or its
        // entry points all miss. Retrieval is forwarding-Kademlia with no
        // authoritative negative on the wire: a failed or slow leg means "this
        // entry point could not serve it", never "the chunk is absent", so the
        // walk keeps going and only gives up on a real bound (the leg budget, the
        // pool, or the deadline). Staggering one leg in at a time bounds the paid
        // fan-out; each leg reserves the per-peer in-flight permit that rides its
        // future, released on drop including a cancelled losing leg.
        let closest_peers = self
            .topology
            .closest_to(&chunk_address, RETRIEVE_WALK_WIDTH);
        // Settle-and-await when every close peer is gated, so a fully gated set
        // recovers instead of failing; if still gated at the budget the empty
        // result falls through to the no-connected-peers path below, the same
        // generic transient error a no-peers failure yields.
        let closest_peers = self.select_or_wait(closest_peers, &chunk_address).await;
        let (candidates, enforce_cap) = skip_busy(closest_peers, self.inflight.as_deref());

        let outcome = self
            .walk_legs(
                candidates,
                chunk_address,
                WalkBounds {
                    budget: RETRIEVE_LEG_BUDGET,
                    max_in_flight: RETRIEVE_WALK_MAX_IN_FLIGHT,
                    deadline: RETRIEVE_WALK_DEADLINE,
                    stagger: RETRIEVAL_STAGGER,
                },
                enforce_cap,
                &legs,
            )
            .await;

        let legs = legs.load(Ordering::Relaxed);
        histogram!("retrieval_walk_legs").record(legs as f64);
        let outcome_label = match &outcome {
            Ok(_) => "hit",
            Err(RaceFailure::NoCandidates) => "no_peers",
            Err(RaceFailure::AllFailed(_)) => "exhausted",
            Err(RaceFailure::TimedOut) => "timed_out",
        };
        counter!("retrieval_walk_total", "outcome" => outcome_label, "path" => "fallback")
            .increment(1);

        match outcome {
            Ok(result) => Ok(ChunkRetrievalResult {
                chunk: result.chunk,
                stamp: result.stamp,
                served_by: result.peer,
            }),
            Err(RaceFailure::NoCandidates) => Err(SwarmError::network_msg(
                "no connected peers available for retrieval",
            )),
            Err(RaceFailure::AllFailed(e)) => Err(SwarmError::AllPeersFailed {
                address: *address,
                attempts: legs,
                source: Box::new(e),
            }),
            // The walk ran out of wall-clock time with legs still slow rather
            // than failed: a transient condition, surfaced like a no-peers miss
            // so the consumer re-streams the address rather than treating it as a
            // hard not-found.
            Err(RaceFailure::TimedOut) => Err(SwarmError::network_msg(
                "retrieval walk deadline elapsed before any peer served the chunk",
            )),
        }
    }

    fn has_chunk(&self, _address: &ChunkAddress) -> bool {
        // Client nodes don't have local storage
        false
    }
}

impl<I: SwarmIdentity> NetworkChunkProvider<I> {
    /// Push `chunk` to the storer peers closest to its address, returning the
    /// first receipt.
    ///
    /// Walks the closest candidates in order and returns the first storer that
    /// accepts the chunk. The client handle correlates a push response to its
    /// request by chunk address alone, so the candidates are tried sequentially
    /// rather than raced.
    async fn push_to_closest(&self, chunk: StampedChunk) -> SwarmResult<PushReceipt> {
        let address = *chunk.address();
        let closest = self.topology.closest_to(&address, PUSH_CANDIDATE_COUNT);
        // Settle-and-await an all-gated closest set rather than immediately
        // falling through to a farther peer or failing; if still gated at the
        // budget the empty result yields the generic no-storer outcome below.
        let closest = self.select_or_wait(closest, &address).await;
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
        let reporter = self.topology.peer_manager();

        // Try each closest peer in order and return the first receipt that
        // verifies. A shallow receipt is rejected, the responding peer scored
        // adversely, and the walk continues to the next candidate: this is the
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
                        reporter,
                    ) {
                        DepthVerdict::Verified => return Ok(push_receipt_of(receipt)),
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
}

/// Build the bin-bucket proximity-routed candidate order for `chunk`.
///
/// Routes to the Kademlia forwarding bin `b = PO(local, chunk)` first: a peer
/// there shares the chunk's first `b` bits, so it is at least as close to the
/// chunk as we are and forwards over fewer hops. When that bin yields too few
/// peers the route spills outward to the adjacent bins (`b-1`, `b+1`, `b-2`, ...)
/// up to `width` candidates, so a sparse forwarding bin still finds an entry
/// point. Within every bin the peers are ordered closest-to-chunk first.
/// `peers_in_bin` returns connected peers only; retrieval never dials.
fn bin_routed_order(
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
/// outward to the nearest bins on either side (`b-1`, `b+1`, `b-2`, `b+2`, ...)
/// within `[0, max_bin]`.
fn spill_bins(b: u8, max_bin: u8) -> Vec<u8> {
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

/// Filter proximity-ordered `candidates` to those with a free retrieval slot,
/// returning the survivors and whether the walk should enforce the cap.
///
/// With no limiter the list is unchanged and `enforce_cap` is false. When every
/// close candidate is at its in-flight cap the full list is returned with
/// `enforce_cap` false (fall through, since degraded service beats failing the
/// request, so legs run best-effort). Only when free-slot peers are found is
/// `enforce_cap` true, so a peer that fills between this snapshot and dispatch is
/// declined rather than run uncapped. Skip-busy happens here, at selection time,
/// so a busy head peer is never raced and the next-closest free peer leads.
fn skip_busy(
    candidates: Vec<SwarmAddress>,
    inflight: Option<&PeerInflightLimiter>,
) -> (Vec<SwarmAddress>, bool) {
    let Some(limiter) = inflight else {
        return (candidates, false);
    };
    let survivors: Vec<SwarmAddress> = candidates
        .iter()
        .copied()
        .filter(|peer| limiter.has_free_slot(peer))
        .collect();
    // `enforce_cap` only when free-slot peers were found: the all-busy
    // fall-through returns the full list so the walk still attempts, best-effort.
    if survivors.is_empty() {
        (candidates, false)
    } else {
        (survivors, true)
    }
}

/// Project the internal domain [`Receipt`] onto the public boundary
/// [`PushReceipt`] returned to operators and embedders.
fn push_receipt_of(receipt: Receipt) -> PushReceipt {
    PushReceipt {
        storer: receipt.storer,
        signature: receipt.signature,
        nonce: receipt.nonce,
        storage_radius: receipt.storage_radius,
    }
}

#[async_trait]
impl<I: SwarmIdentity> SwarmChunkSender for NetworkChunkProvider<I> {
    async fn send_chunk_unchecked(&self, chunk: StampedChunk) -> SwarmResult<PushReceipt> {
        self.push_to_closest(chunk).await
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

        self.push_to_closest(chunk).await
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
///   reporter (the same path #287 uses), and the caller retries via a different
///   route instead of believing a fabricated shallow receipt.
/// - [`DepthVerdict::Unverifiable`]: the local view is not credible enough to
///   judge custody depth. The peer is NOT penalised (it may be honest); the
///   caller treats the push as unconfirmed.
fn accept_origin_receipt(
    receipt: &Receipt,
    peer: SwarmAddress,
    local_depth: vertex_swarm_api::NeighborhoodDepth,
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
    use std::sync::Mutex;

    use alloy_signer::SignerSync;
    use alloy_signer_local::PrivateKeySigner;
    use nectar_primitives::{Bin, NetworkId, Nonce, compute_overlay};
    use vertex_swarm_api::{NeighborhoodDepth, ReportSource, StorageRadius, SwarmScoringEvent};
    use vertex_swarm_net_pushsync::WireReceipt;

    use super::*;

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
    /// storer ground to sit at least `min_depth` bits deep relative to `address`.
    fn signed_receipt(
        signer: &PrivateKeySigner,
        address: &ChunkAddress,
        min_depth: u8,
        storage_radius: StorageRadius,
    ) -> Receipt {
        let eth = signer.address();
        // The signature is over the 32-byte address only (the wire format) and
        // is independent of the nonce, so sign once and grind for overlay depth.
        let signature = signer.sign_message_sync(address.as_bytes()).expect("sign");
        let mut counter = 0u64;
        loop {
            let mut nonce_bytes = [0u8; 32];
            nonce_bytes[..8].copy_from_slice(&counter.to_le_bytes());
            let nonce = Nonce::from(nonce_bytes);
            let overlay = compute_overlay(&eth, NET, &nonce);
            if address.proximity(&overlay).get() >= min_depth {
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
        // Regression for #316: with a non-credible local view (a fresh or sparse
        // node, local_depth == 0) a shallow receipt declaring radius 0 must NOT
        // be accepted, and the responder must NOT be penalised: the verdict is
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

    mod staggered_race {
        use std::time::{Duration, Instant};

        use crate::{ChunkTransferError, ClientCommand, ClientHandle, RetrievalResult};
        use nectar_primitives::ContentChunk;
        use tokio::sync::mpsc;

        use crate::race_candidates;

        use super::super::{RETRIEVAL_STAGGER, RaceFailure};
        use super::*;

        fn test_chunk() -> nectar_primitives::AnyChunk {
            ContentChunk::new(&b"provider-race-chunk"[..])
                .expect("valid content chunk")
                .into()
        }

        /// Drive the exact future the provider builds per candidate: each
        /// attempt is `client_handle.retrieve_chunk(peer, address)`, raced with a
        /// staggered start. The per-candidate pacing (the admission band and
        /// affordability check) lives inside that call, so this exercises the
        /// provider's retrieval leg and race wiring without standing up a
        /// topology mock.
        async fn race_over_handle(
            handle: ClientHandle,
            candidates: Vec<SwarmAddress>,
            address: ChunkAddress,
        ) -> Result<RetrievalResult, RaceFailure<ChunkTransferError>> {
            race_candidates(candidates, RETRIEVAL_STAGGER, move |peer| {
                let handle = handle.clone();
                async move { handle.retrieve_chunk(peer, address, true).await }
            })
            .await
        }

        #[tokio::test]
        async fn withholding_head_is_overtaken_by_the_second_candidate() {
            let (tx, mut rx) = mpsc::channel::<ClientCommand>(16);
            let handle = ClientHandle::new(tx);

            let address = address(0xaa);
            let peer_a = SwarmAddress::from([1u8; 32]);
            let peer_b = SwarmAddress::from([2u8; 32]);

            let start = Instant::now();
            let race = tokio::spawn(race_over_handle(handle, vec![peer_a, peer_b], address));

            // The head request arrives first; leave it unanswered so it
            // withholds. The stagger must bring in the second candidate, whose
            // response resolves the race well under the per-attempt deadline.
            let head = match rx.recv().await.expect("head command") {
                ClientCommand::RetrieveChunk { peer, response, .. } => {
                    assert_eq!(peer, peer_a);
                    response
                }
                other => panic!("unexpected command: {other:?}"),
            };
            match rx.recv().await.expect("second command after stagger") {
                ClientCommand::RetrieveChunk { peer, response, .. } => {
                    assert_eq!(peer, peer_b);
                    response
                        .send(Ok(RetrievalResult {
                            chunk: test_chunk(),
                            stamp: None,
                            peer: peer_b,
                        }))
                        .expect("receiver alive");
                }
                other => panic!("unexpected command: {other:?}"),
            }

            let result = race.await.unwrap().expect("race resolves");
            assert_eq!(result.peer, peer_b, "the staggered second wins");
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "overtaken within the stagger, well under the per-attempt deadline"
            );

            // The losing head request's response channel was dropped when the
            // race resolved: the handler observes the closed receiver and
            // releases any reservation the in-flight attempt held. Sending on it
            // now fails, proving the loser was dropped (not run to completion).
            assert!(
                head.send(Ok(RetrievalResult {
                    chunk: test_chunk(),
                    stamp: None,
                    peer: peer_a,
                }))
                .is_err(),
                "the losing head response channel is dropped on resolve"
            );
        }

        #[tokio::test]
        async fn all_candidates_failing_yields_the_last_error() {
            // The handle's command channel is closed, so every retrieval attempt
            // fails immediately and the race exhausts every candidate.
            let (tx, rx) = mpsc::channel::<ClientCommand>(16);
            drop(rx);
            let handle = ClientHandle::new(tx);

            let address = address(0xbb);
            let candidates = vec![SwarmAddress::from([1u8; 32]), SwarmAddress::from([2u8; 32])];

            let outcome = race_over_handle(handle, candidates, address).await;
            assert!(
                matches!(
                    outcome,
                    Err(RaceFailure::AllFailed(ChunkTransferError::ChannelClosed))
                ),
                "all candidates failing surfaces the last attempt's error"
            );
        }

        #[tokio::test]
        async fn no_candidates_yields_no_candidates() {
            let (tx, _rx) = mpsc::channel::<ClientCommand>(16);
            let handle = ClientHandle::new(tx);

            let outcome = race_over_handle(handle, Vec::new(), address(0xcc)).await;
            assert!(matches!(outcome, Err(RaceFailure::NoCandidates)));
        }
    }

    mod skip_busy_scheduler {
        use std::num::NonZeroUsize;
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        use nectar_primitives::ContentChunk;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use tokio::sync::mpsc;

        use crate::{
            ChunkTransferError, ClientCommand, ClientHandle, PeerInflightLimiter, RetrievalResult,
        };

        use crate::race_candidates;

        use super::super::{
            RETRIEVAL_STAGGER, RETRIEVE_LEG_BUDGET, RETRIEVE_WALK_DEADLINE, RaceFailure, race_walk,
            skip_busy,
        };
        use super::*;

        const CAP_ONE: NonZeroUsize = match NonZeroUsize::new(1) {
            Some(cap) => cap,
            None => unreachable!(),
        };

        fn overlay(n: u8) -> SwarmAddress {
            SwarmAddress::from([n; 32])
        }

        fn test_chunk() -> nectar_primitives::AnyChunk {
            ContentChunk::new(&b"skip-busy-chunk"[..])
                .expect("valid content chunk")
                .into()
        }

        /// Drive the exact composition the provider builds: skip-busy filtering
        /// at selection time, then the staggered race whose legs reserve an
        /// in-flight permit that rides the request future and releases on drop.
        async fn race_with_limiter(
            handle: ClientHandle,
            limiter: Arc<PeerInflightLimiter>,
            candidates: Vec<SwarmAddress>,
            address: ChunkAddress,
        ) -> Result<RetrievalResult, RaceFailure<ChunkTransferError>> {
            let (candidates, _enforce_cap) = skip_busy(candidates, Some(&limiter));
            race_candidates(candidates, RETRIEVAL_STAGGER, move |peer| {
                let permit = limiter.try_acquire(&peer);
                let handle = handle.clone();
                async move {
                    let _permit = permit;
                    handle.retrieve_chunk(peer, address, true).await
                }
            })
            .await
        }

        #[test]
        fn skip_busy_without_a_limiter_keeps_every_candidate() {
            let candidates = vec![overlay(1), overlay(2), overlay(3)];
            assert_eq!(skip_busy(candidates.clone(), None), (candidates, false));
        }

        #[test]
        fn skip_busy_drops_a_capped_head() {
            let limiter = PeerInflightLimiter::new(CAP_ONE);
            let busy = overlay(1);
            let _held = limiter.try_acquire(&busy).expect("first slot");

            let (survivors, enforce_cap) =
                skip_busy(vec![busy, overlay(2), overlay(3)], Some(&limiter));
            assert_eq!(
                survivors,
                vec![overlay(2), overlay(3)],
                "the capped head is skipped, the next-closest free peers remain"
            );
            assert!(enforce_cap, "free-slot peers found, so the cap is enforced");
        }

        #[test]
        fn skip_busy_falls_through_when_every_candidate_is_capped() {
            let limiter = PeerInflightLimiter::new(CAP_ONE);
            let candidates = vec![overlay(1), overlay(2)];
            let _h1 = limiter.try_acquire(&overlay(1)).expect("slot a");
            let _h2 = limiter.try_acquire(&overlay(2)).expect("slot b");

            let (survivors, enforce_cap) = skip_busy(candidates.clone(), Some(&limiter));
            assert_eq!(
                survivors, candidates,
                "all-busy falls through to the full list rather than failing"
            );
            assert!(
                !enforce_cap,
                "the all-busy fall-through is best-effort, not cap-enforced"
            );
        }

        #[tokio::test]
        async fn walk_budget_caps_metered_legs_below_the_free_slot_pool() {
            // A wide pool of free-slot peers must not meter a leg each: the walk
            // dispatches at most the leg budget, refilling a failed leg from the
            // next-closest peer, so the wider pool supplies coverage alternatives
            // without amplifying paid bandwidth.
            let (tx, rx) = mpsc::channel::<ClientCommand>(64);
            drop(rx); // every retrieval fails at once: the walk spends its budget.
            let handle = ClientHandle::new(tx);
            let limiter = Arc::new(PeerInflightLimiter::new(CAP_ONE));

            let pool: Vec<SwarmAddress> = (1..=16).map(overlay).collect();
            let (candidates, _enforce_cap) = skip_busy(pool, Some(&limiter));
            assert_eq!(candidates.len(), 16, "all 16 peers have a free slot");

            let legs = Arc::new(AtomicUsize::new(0));
            let counted = Arc::clone(&legs);
            let outcome = race_walk(
                candidates,
                RETRIEVE_LEG_BUDGET,
                RETRIEVE_WALK_MAX_IN_FLIGHT,
                RETRIEVE_WALK_DEADLINE,
                RETRIEVAL_STAGGER,
                move |peer| {
                    counted.fetch_add(1, Ordering::SeqCst);
                    let permit = limiter.try_acquire(&peer);
                    let handle = handle.clone();
                    Some(async move {
                        let _permit = permit;
                        handle.retrieve_chunk(peer, address(0xaa), true).await
                    })
                },
            )
            .await;

            assert!(matches!(outcome, Err(RaceFailure::AllFailed(_))));
            assert_eq!(
                legs.load(Ordering::SeqCst),
                RETRIEVE_LEG_BUDGET,
                "the walk meters at most the leg budget across the wider free-slot pool"
            );
        }

        #[tokio::test]
        async fn enforce_cap_declines_a_peer_that_filled_since_the_snapshot() {
            // A peer free at the skip-busy snapshot but saturated before its leg
            // dispatches is declined under enforce_cap: no command reaches it and
            // it spends no leg, so the cap holds on live state, not the stale
            // snapshot. The next free peer serves instead.
            let (tx, mut rx) = mpsc::channel::<ClientCommand>(16);
            let handle = ClientHandle::new(tx);
            let limiter = Arc::new(PeerInflightLimiter::new(CAP_ONE));

            let filled = overlay(1);
            let free = overlay(2);

            // Skip-busy sees both peers free, so the walk enforces the cap.
            let (candidates, enforce_cap) = skip_busy(vec![filled, free], Some(&limiter));
            assert_eq!(candidates, vec![filled, free]);
            assert!(enforce_cap, "free-slot peers found, so the cap is enforced");

            // Between the snapshot and dispatch the first peer's slot is taken.
            let _held = limiter
                .try_acquire(&filled)
                .expect("saturate the first peer");

            let address = address(0xac);
            let legs = Arc::new(AtomicUsize::new(0));
            let counted = Arc::clone(&legs);
            let lim = Arc::clone(&limiter);
            let race = tokio::spawn(async move {
                race_walk(
                    candidates,
                    RETRIEVE_LEG_BUDGET,
                    RETRIEVE_WALK_MAX_IN_FLIGHT,
                    RETRIEVE_WALK_DEADLINE,
                    RETRIEVAL_STAGGER,
                    move |peer| {
                        let permit = lim.try_acquire(&peer);
                        // The enforce-cap decline: a peer with no live slot spends
                        // no leg and is skipped for the next candidate.
                        if enforce_cap && permit.is_none() {
                            return None;
                        }
                        counted.fetch_add(1, Ordering::SeqCst);
                        let handle = handle.clone();
                        Some(async move {
                            let _permit = permit;
                            handle.retrieve_chunk(peer, address, true).await
                        })
                    },
                )
                .await
            });

            // The only command is for the free peer: the saturated peer is declined.
            match rx.recv().await.expect("a command for the free peer") {
                ClientCommand::RetrieveChunk { peer, response, .. } => {
                    assert_eq!(peer, free, "the saturated peer is declined, not contacted");
                    response
                        .send(Ok(RetrievalResult {
                            chunk: test_chunk(),
                            stamp: None,
                            peer: free,
                        }))
                        .expect("receiver alive");
                }
                other => panic!("unexpected command: {other:?}"),
            }

            let result = race.await.unwrap().expect("race resolves");
            assert_eq!(result.peer, free, "the free peer serves the chunk");
            assert_eq!(
                legs.load(Ordering::SeqCst),
                1,
                "only the free peer spent a leg; the saturated peer was declined"
            );
        }

        #[tokio::test]
        async fn capped_head_is_skipped_for_the_next_free_peer() {
            // The closest peer is at its cap; the race must dispatch to the
            // next-closest peer with a free slot, never blocking on or contacting
            // the capped head.
            let (tx, mut rx) = mpsc::channel::<ClientCommand>(16);
            let handle = ClientHandle::new(tx);
            let limiter = Arc::new(PeerInflightLimiter::new(CAP_ONE));

            let head = overlay(1);
            let next = overlay(2);
            // Saturate the head so it has no free slot at selection time.
            let _held = limiter.try_acquire(&head).expect("saturate the head");

            let address = address(0xab);
            let race = tokio::spawn(race_with_limiter(
                handle,
                Arc::clone(&limiter),
                vec![head, next],
                address,
            ));

            // The only command dispatched is to the next-closest peer: the capped
            // head was skipped at selection time, not contacted.
            match rx.recv().await.expect("a command for the free peer") {
                ClientCommand::RetrieveChunk { peer, response, .. } => {
                    assert_eq!(peer, next, "the skipped head is not contacted");
                    response
                        .send(Ok(RetrievalResult {
                            chunk: test_chunk(),
                            stamp: None,
                            peer: next,
                        }))
                        .expect("receiver alive");
                }
                other => panic!("unexpected command: {other:?}"),
            }

            let result = race.await.unwrap().expect("race resolves");
            assert_eq!(result.peer, next, "the free next-closest peer serves");
        }

        #[tokio::test]
        async fn losing_leg_releases_its_permit_on_drop() {
            // The head leg reserves a permit and then withholds; the staggered
            // second wins and the head leg is dropped. Dropping it must release
            // the head's in-flight slot, so the head is reservable again.
            let (tx, mut rx) = mpsc::channel::<ClientCommand>(16);
            let handle = ClientHandle::new(tx);
            let limiter = Arc::new(PeerInflightLimiter::new(CAP_ONE));

            let head = overlay(1);
            let second = overlay(2);
            let address = address(0xcd);

            let start = Instant::now();
            let race = tokio::spawn(race_with_limiter(
                handle,
                Arc::clone(&limiter),
                vec![head, second],
                address,
            ));

            // The head leg dispatches first and reserves the head's only slot.
            let _head_response = match rx.recv().await.expect("head command") {
                ClientCommand::RetrieveChunk { peer, response, .. } => {
                    assert_eq!(peer, head);
                    assert!(
                        !limiter.has_free_slot(&head),
                        "the in-flight head leg holds the head's slot"
                    );
                    response
                }
                other => panic!("unexpected command: {other:?}"),
            };
            // After the stagger the second candidate joins and resolves the race.
            match rx.recv().await.expect("second command after stagger") {
                ClientCommand::RetrieveChunk { peer, response, .. } => {
                    assert_eq!(peer, second);
                    response
                        .send(Ok(RetrievalResult {
                            chunk: test_chunk(),
                            stamp: None,
                            peer: second,
                        }))
                        .expect("receiver alive");
                }
                other => panic!("unexpected command: {other:?}"),
            }

            let result = race.await.unwrap().expect("race resolves");
            assert_eq!(result.peer, second, "the staggered second wins");
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "resolved within the stagger, not a per-attempt deadline"
            );
            // The losing head leg was dropped when the race resolved, releasing
            // its permit: the head's slot is free again.
            assert!(
                limiter.has_free_slot(&head),
                "the cancelled head leg released its in-flight slot on drop"
            );
            assert!(
                limiter.try_acquire(&head).is_some(),
                "the freed head slot is reservable again"
            );
        }
    }

    mod gated_fallback {
        use std::future::Future;
        use std::pin::Pin;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        use nectar_primitives::SwarmAddress;
        use vertex_swarm_api::{Au, ChunkAddress, Ledger, SwarmError, SwarmPricing};
        use vertex_tasks::time::Instant;

        use crate::{PeerScores, PeerSelector, SettlementTrigger};

        use super::address;

        fn overlay(n: u8) -> SwarmAddress {
            SwarmAddress::from([n; 32])
        }

        struct NoScores;
        impl PeerScores for NoScores {
            fn peer_score(&self, _overlay: &SwarmAddress) -> Option<f64> {
                None
            }
        }

        struct UnitPricer;
        impl SwarmPricing for UnitPricer {
            fn price(&self, _chunk: &ChunkAddress) -> Au {
                Au::from_amount(1)
            }
            fn peer_price(&self, _peer: &SwarmAddress, _chunk: &ChunkAddress) -> Au {
                Au::from_amount(1)
            }
        }

        /// Refuses every peer at the unit price while `gated`; admits once clear.
        struct GatedLedger(Arc<AtomicBool>);
        impl Ledger for GatedLedger {
            fn balance(&self, _o: &SwarmAddress) -> Au {
                Au::ZERO
            }
            fn reserved(&self, _o: &SwarmAddress) -> Au {
                Au::ZERO
            }
            fn disconnect_line(&self, _o: &SwarmAddress) -> Au {
                if self.0.load(Ordering::SeqCst) {
                    Au::ZERO
                } else {
                    Au::from_amount(1000)
                }
            }
            fn settle_trigger(&self, _o: &SwarmAddress) -> Au {
                Au::from_amount(1000)
            }
        }

        /// `settled` resolves at once; when `drains` it clears the gate first
        /// (modelling a completed in-flight settle that drops the peer's debt
        /// under its line) and reports `true`. Otherwise it reports `false`: no
        /// settle is draining debt, so the wait loop stops without spinning.
        struct DrainSettlement {
            gate: Arc<AtomicBool>,
            drains: bool,
        }
        impl SettlementTrigger for DrainSettlement {
            fn trigger_settlement(&self, _peer: SwarmAddress) {}
            fn settled(
                &self,
                _peers: &[SwarmAddress],
            ) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
                if self.drains {
                    self.gate.store(false, Ordering::SeqCst);
                }
                Box::pin(std::future::ready(self.drains))
            }
        }

        fn selector(gate: Arc<AtomicBool>, drains: bool) -> PeerSelector {
            PeerSelector::new(
                Arc::new(NoScores),
                Arc::new(GatedLedger(Arc::clone(&gate))),
                Arc::new(UnitPricer),
                Arc::new(DrainSettlement { gate, drains }),
            )
        }

        #[tokio::test]
        async fn a_recovering_gated_set_returns_the_admissible_peers() {
            // Every close peer is gated on the first order; the awaited settle
            // drains the debt (the gate clears) and the re-order returns them.
            let gate = Arc::new(AtomicBool::new(true));
            let sel = selector(Arc::clone(&gate), true);
            let deadline = Instant::now() + std::time::Duration::from_secs(5);
            let ordered = sel
                .order_or_wait(
                    vec![overlay(1), overlay(2)],
                    &ChunkAddress::zero(),
                    deadline,
                )
                .await;
            assert_eq!(
                ordered,
                vec![overlay(1), overlay(2)],
                "recovers once the settle drains"
            );
        }

        #[tokio::test]
        async fn a_fully_gated_set_with_no_draining_settle_returns_empty() {
            // The gate never clears and no settle is draining debt: the
            // no-progress guard terminates the wait with an empty result, so the
            // dispatch falls through to its generic transient error rather than
            // hanging or spinning to the far deadline.
            let gate = Arc::new(AtomicBool::new(true));
            let sel = selector(Arc::clone(&gate), false);
            let deadline = Instant::now() + std::time::Duration::from_secs(30);
            let ordered = sel
                .order_or_wait(
                    vec![overlay(1), overlay(2)],
                    &ChunkAddress::zero(),
                    deadline,
                )
                .await;
            assert!(ordered.is_empty(), "still gated, no settle draining");
        }

        #[test]
        fn an_empty_close_set_surfaces_a_generic_transient_error() {
            // What a fully gated (empty) selection falls through to is the same
            // generic transient failure a genuine no-peers/no-storer case yields:
            // retrieval a `Network` error, push a `NoStorer` error. Neither is an
            // accounting-specific variant, so the accounting concern never reaches
            // the consumer.
            let retrieval = SwarmError::network_msg("no connected peers available for retrieval");
            assert!(matches!(retrieval, SwarmError::Network { .. }));
            assert!(
                retrieval.is_retryable(),
                "a no-peers retrieval is transient"
            );

            let push = SwarmError::NoStorer {
                chunk_address: address(0xaa),
            };
            assert!(matches!(push, SwarmError::NoStorer { .. }));
            assert!(push.is_retryable(), "a no-storer push is transient");
        }
    }
}
