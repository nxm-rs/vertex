//! Score- and credit-aware peer selection for retrieval and pushsync.
//!
//! Topology returns candidate storers in proximity order. [`PeerSelector`]
//! reorders them with three additional signals before a request goes out:
//!
//! - A peer the admission band refuses (the per-chunk debit would cross its
//!   disconnect line) is hard-skipped, so the request routes to the next-closest
//!   affordable peer while a background settle drains the skipped one.
//! - Peers whose score is in the warned range are excluded among the admissible,
//!   so a peer that is being scored down is not asked again while it misbehaves.
//! - The admissible peers split into two affordability tiers: those with full
//!   forgiveness headroom (the band still [`Admit`]s) lead those already past the
//!   settle trigger (the band [`SettleAndAdmit`]s). A bulk download therefore
//!   spreads its debt across the closest headroom peers before it leans on a
//!   near-threshold one, so no single close peer's free allowance saturates while
//!   the aggregate forgiveness across the neighbourhood stays untapped.
//!
//! Proximity is the secondary key within each tier; the headroom split orders the
//! admissible above it. If every admissible candidate is warned, the warned peers
//! are returned in proximity order so a degraded request can still go out; a
//! refused peer is never resurrected this way. Every candidate not plainly
//! admitted is settled (a peer in the tolerance band to stay there, a refused
//! peer to drain back under its line); the settlement providers themselves decide
//! whether any payment is actually due.
//!
//! [`Admit`]: Admission::Admit
//! [`SettleAndAdmit`]: Admission::SettleAndAdmit
//!
//! The price consulted per candidate is [`SwarmPricing::peer_price`], the same
//! per-peer chunk price the accounting layer debits when the request is
//! served.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use futures::FutureExt;
use futures::future::Shared;
use nectar_primitives::ChunkAddress;
use parking_lot::Mutex;
use rustc_hash::FxBuildHasher;
use tokio::sync::oneshot;
use tracing::debug;
use vertex_swarm_api::{
    Admission, AdmissionControl, DEFAULT_PEER_WARN_THRESHOLD, SwarmBandwidthAccounting,
    SwarmIdentity, SwarmPeerBandwidth, SwarmPricing,
};
use vertex_swarm_primitives::OverlayAddress;
use vertex_swarm_topology::TopologyHandle;
use vertex_tasks::TaskExecutor;
use vertex_tasks::time::Instant;

/// A shareable handle that resolves when an in-flight settle for a peer
/// completes (the settle's [`InFlightGuard`] fires it on drop), so a waiter can
/// re-evaluate admission as soon as the network settle acks.
type SettleCompletion = Shared<oneshot::Receiver<()>>;

/// Per-peer in-flight settle map. Overlay keys are uniformly random, so a fast
/// non-DoS hasher keeps the settle-trigger dedup off SipHash.
type InFlightMap = HashMap<OverlayAddress, SettleCompletion, FxBuildHasher>;

/// Source of live peer scores for candidate selection.
///
/// Implemented over the topology handle (which queries the peer manager, the
/// authority that owns peer records) and by test mocks.
#[auto_impl::auto_impl(&, Arc)]
pub trait PeerScores: Send + Sync {
    /// Current score for the peer, or `None` when the peer is unknown.
    fn peer_score(&self, overlay: &OverlayAddress) -> Option<f64>;
}

impl<I: SwarmIdentity> PeerScores for TopologyHandle<I> {
    fn peer_score(&self, overlay: &OverlayAddress) -> Option<f64> {
        self.peer_manager().get_peer_score(overlay)
    }
}

/// Best-effort settlement trigger for a peer whose debt has reached the band.
///
/// Implemented over bandwidth accounting (see [`AccountingSettlement`]) and by
/// test mocks.
#[auto_impl::auto_impl(&, Arc)]
pub trait SettlementTrigger: Send + Sync {
    /// Start settlement with `peer` without waiting for the outcome.
    fn trigger_settlement(&self, peer: OverlayAddress);

    /// Resolve once any in-flight settle for the subset of `peers` currently
    /// settling has completed, yielding whether one was actually awaited.
    ///
    /// Returns `true` after awaiting a real in-flight settle completion (paced by
    /// network RTT), `false` immediately when none of `peers` is in flight. The
    /// `bool` keeps the future `Send` and lets a gated-set wait loop both pace on
    /// the settle ack and detect when no progress is possible (nothing draining
    /// debt), so it returns rather than spinning to the deadline.
    fn settled(&self, peers: &[OverlayAddress]) -> Pin<Box<dyn Future<Output = bool> + Send + '_>>;
}

/// [`SettlementTrigger`] over a bandwidth accounting instance.
///
/// Settlement runs as a spawned task on the current task executor so
/// selection never blocks on settlement I/O. Failures are logged at debug
/// level; the next request retries naturally.
///
/// A shared in-flight map keeps at most one settle per peer running at a time:
/// the second trigger for a peer with a settle still outstanding is dropped, so
/// the map is both the dedup and the per-peer rate limit (the next settle cannot
/// start until the prior one is acked). The stored [`SettleCompletion`] lets a
/// waiter await that ack.
pub struct AccountingSettlement<B> {
    bandwidth: B,
    in_flight: Arc<Mutex<InFlightMap>>,
}

impl<B> AccountingSettlement<B> {
    /// Trigger settlement through `bandwidth`.
    pub fn new(bandwidth: B) -> Self {
        Self {
            bandwidth,
            in_flight: Arc::new(Mutex::new(InFlightMap::default())),
        }
    }
}

/// Removes a peer from the in-flight map on drop and fires its completion, so a
/// panic or cancellation of the settle future cannot pin the peer (which would
/// starve its settlement) and any waiter on the settle is woken either way.
struct InFlightGuard {
    in_flight: Arc<Mutex<InFlightMap>>,
    peer: OverlayAddress,
    done: Option<oneshot::Sender<()>>,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.in_flight.lock().remove(&self.peer);
        // Fulfil the completion; dropping the sender would also resolve the
        // shared receiver, so a waiter is woken on completion, cancellation, and
        // panic alike.
        if let Some(done) = self.done.take() {
            let _ = done.send(());
        }
    }
}

impl<B> SettlementTrigger for AccountingSettlement<B>
where
    B: SwarmBandwidthAccounting + 'static,
    B::Peer: 'static,
{
    fn trigger_settlement(&self, peer: OverlayAddress) {
        let Ok(executor) = TaskExecutor::try_current() else {
            debug!(%peer, "no task executor; settlement not triggered");
            return;
        };
        // Skip if a settle to this peer is already running; the entry is cleared
        // when the spawned settle completes, so the next trigger can start a fresh
        // one. The common steady-state path re-triggers a peer whose settle is
        // still in flight, so the dedup check happens before the oneshot/Shared
        // allocation: a hit returns on a bare map probe with nothing allocated.
        let done = {
            let mut in_flight = self.in_flight.lock();
            if in_flight.contains_key(&peer) {
                return;
            }
            let (done, completion) = oneshot::channel();
            in_flight.insert(peer, completion.shared());
            done
        };
        let handle = self.bandwidth.for_peer(peer);
        let guard = InFlightGuard {
            in_flight: Arc::clone(&self.in_flight),
            peer,
            done: Some(done),
        };
        executor.spawn(async move {
            let _guard = guard;
            if let Err(error) = handle.settle().await {
                debug!(%peer, %error, "best-effort settlement failed");
            }
        });
    }

    fn settled(&self, peers: &[OverlayAddress]) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
        let waiters: Vec<SettleCompletion> = {
            let in_flight = self.in_flight.lock();
            peers
                .iter()
                .filter_map(|peer| in_flight.get(peer).cloned())
                .collect()
        };
        Box::pin(async move {
            if waiters.is_empty() {
                return false;
            }
            // The first completion is enough to re-evaluate admission.
            let _ = futures::future::select_all(waiters).await;
            true
        })
    }
}

/// Reorders proximity-ordered candidates by the admission band and score.
///
/// Built by the node assembly from the topology handle (scores), bandwidth
/// accounting (the admission band and settlement), and the pricer (per-peer
/// chunk price). Consumed by the retrieval and pushsync candidate-selection
/// paths.
pub struct PeerSelector {
    scores: Arc<dyn PeerScores>,
    admission: Arc<dyn AdmissionControl>,
    pricing: Arc<dyn SwarmPricing>,
    settlement: Arc<dyn SettlementTrigger>,
}

impl PeerSelector {
    /// Compose a selector from its query and trigger surfaces.
    pub fn new(
        scores: Arc<dyn PeerScores>,
        admission: Arc<dyn AdmissionControl>,
        pricing: Arc<dyn SwarmPricing>,
        settlement: Arc<dyn SettlementTrigger>,
    ) -> Self {
        Self {
            scores,
            admission,
            pricing,
            settlement,
        }
    }

    /// Order `candidates` (in proximity order) for a request on `chunk`.
    ///
    /// Applies the ranking described at the module level: a [`Refuse`] candidate
    /// is hard-skipped (sending would cross its disconnect line), warned peers are
    /// excluded, and the admissible rest are tiered headroom-first (an [`Admit`]
    /// peer leads a [`SettleAndAdmit`] one) with proximity the key within a tier.
    /// Every candidate not plainly [`Admit`]ted is settled (a [`SettleAndAdmit`]
    /// peer to stay in the band, a refused peer to drain back under its line), so
    /// its view of our debt drops.
    /// A single pass triggers each peer at most once; the in-flight set dedups
    /// across repeated calls.
    ///
    /// [`Refuse`]: Admission::Refuse
    /// [`Admit`]: Admission::Admit
    /// [`SettleAndAdmit`]: Admission::SettleAndAdmit
    pub fn order(
        &self,
        candidates: Vec<OverlayAddress>,
        chunk: &ChunkAddress,
    ) -> Vec<OverlayAddress> {
        self.order_inner(candidates, chunk, Tiering::SpreadDebt)
    }

    /// Order `candidates` by proximity among the admissible, dropping the
    /// headroom-first tiering.
    ///
    /// Used by the fully gated retrieval spill, where the goal is the fewest
    /// forwarding hops rather than debt spread: the closest admissible peer leads
    /// even if it is past the settle trigger, so a gated close set spills the
    /// minimum distance instead of skipping near in-band peers for a far headroom
    /// one. Refuse and warned handling, and the settle triggers, match
    /// [`order`](Self::order).
    pub fn order_closest_admissible(
        &self,
        candidates: Vec<OverlayAddress>,
        chunk: &ChunkAddress,
    ) -> Vec<OverlayAddress> {
        self.order_inner(candidates, chunk, Tiering::Proximity)
    }

    fn order_inner(
        &self,
        candidates: Vec<OverlayAddress>,
        chunk: &ChunkAddress,
        tiering: Tiering,
    ) -> Vec<OverlayAddress> {
        // One admission band per candidate drives both the hard-skip (a refused
        // peer leaves `ordered`) and the settle trigger (anything not plainly
        // admitted). The band is read once per candidate (a single ledger lock and
        // key hash via `admit`) and reused for both, so the hot path never bands a
        // peer twice.
        let ranked = rank_candidates(
            &candidates,
            |peer| self.scores.peer_score(peer),
            |peer| {
                self.admission
                    .admit(peer, self.pricing.peer_price(peer, chunk))
            },
            tiering,
        );

        for (peer, band) in candidates.iter().zip(&ranked.bands) {
            if !matches!(band, Admission::Admit) {
                self.settlement.trigger_settlement(*peer);
            }
        }

        ranked.ordered
    }

    /// Order `candidates`, settling and awaiting a fully gated close set until a
    /// peer becomes admissible or the wait is exhausted.
    ///
    /// The first [`order`](Self::order) is the fast path: a non-empty result
    /// returns at once. Only when every candidate is gated (the result is empty)
    /// does the loop wait: each gated `order` already triggered a settle per
    /// peer, so it awaits those settles completing (paced by network RTT, no
    /// timer) and re-orders. The bound is `deadline`, the request's retrieval
    /// lifetime, so accounting-timing back-pressure blocks within the request.
    /// The loop is progress-aware: if no settle is in flight to drain debt
    /// ([`settled`](SettlementTrigger::settled) returns `false`) it returns an
    /// empty result at once rather than spinning, since no re-order can change.
    /// Each awaited settle is RTT-paced, so the loop cannot busy-spin. An empty
    /// result leaves the caller to surface its generic transient failure; an
    /// accounting concern never reaches the consumer. The wait is `Send` on every
    /// platform: awaiting a settle completion and reading [`Instant`] are both
    /// `Send`.
    pub async fn order_or_wait(
        &self,
        candidates: Vec<OverlayAddress>,
        chunk: &ChunkAddress,
        deadline: Instant,
    ) -> Vec<OverlayAddress> {
        loop {
            let ordered = self.order(candidates.clone(), chunk);
            if !ordered.is_empty() {
                return ordered;
            }
            if Instant::now() >= deadline {
                return Vec::new();
            }
            // No in-flight settle means nothing is draining debt, so no re-order
            // can admit a peer: give up now rather than spin to the deadline.
            if !self.settlement.settled(&candidates).await {
                return Vec::new();
            }
        }
    }
}

/// Outcome of ranking a candidate set.
struct RankedCandidates {
    /// Candidates to attempt, best first. Never includes a refused peer.
    ordered: Vec<OverlayAddress>,
    /// The admission band of each input candidate, in input order. Returned so
    /// the caller can drive the settle pass off the same single band read per
    /// candidate rather than re-banding.
    bands: Vec<Admission>,
}

/// How [`rank_candidates`] orders the admissible peers.
#[derive(Clone, Copy)]
enum Tiering {
    /// Headroom-first: an [`Admit`](Admission::Admit) peer leads a
    /// [`SettleAndAdmit`](Admission::SettleAndAdmit) one, so a bulk download
    /// spreads debt across headroom peers before leaning on a near-threshold one.
    /// Proximity is the key within each tier. The close-set order.
    SpreadDebt,
    /// Pure proximity among the admissible, ignoring the headroom split, so the
    /// closest admissible peer leads. The spill order, where fewer forwarding
    /// hops matter more than debt spread.
    Proximity,
}

/// Rank proximity-ordered `candidates` by the admission band and score.
///
/// A [`Refuse`](Admission::Refuse) candidate is hard-skipped:
/// sending would cross its disconnect line, so it never appears in `ordered`.
/// Warned peers (score at or below [`DEFAULT_PEER_WARN_THRESHOLD`]) are excluded
/// among the admissible. The admissible rest are ordered per `tiering`: under
/// [`Tiering::SpreadDebt`] headroom-first (an [`Admit`](Admission::Admit) peer
/// leads a [`SettleAndAdmit`](Admission::SettleAndAdmit) one, proximity the key
/// within each tier), under [`Tiering::Proximity`] in plain proximity order. If
/// every admissible candidate is warned, those warned peers are returned in
/// proximity order so a degraded request can still go out; a refused peer is
/// never resurrected this way. `admit` is invoked exactly once per candidate and
/// the bands are returned for reuse.
fn rank_candidates(
    candidates: &[OverlayAddress],
    score: impl Fn(&OverlayAddress) -> Option<f64>,
    admit: impl Fn(&OverlayAddress) -> Admission,
    tiering: Tiering,
) -> RankedCandidates {
    let mut headroom = Vec::with_capacity(candidates.len());
    let mut near_threshold = Vec::new();
    let mut warned = Vec::new();
    let mut bands = Vec::with_capacity(candidates.len());

    for peer in candidates {
        let band = admit(peer);
        bands.push(band);
        if matches!(band, Admission::Refuse) {
            continue;
        }
        if score(peer).is_some_and(|s| s <= DEFAULT_PEER_WARN_THRESHOLD) {
            warned.push(*peer);
        } else if matches!(tiering, Tiering::SpreadDebt)
            && matches!(band, Admission::SettleAndAdmit)
        {
            near_threshold.push(*peer);
        } else {
            // Under `Proximity` both admissible bands land here, preserving the
            // input proximity order; under `SpreadDebt` only headroom peers do.
            headroom.push(*peer);
        }
    }

    // Headroom (or, under `Proximity`, all admissible) first, near-threshold
    // after, each in proximity order.
    let mut ordered = headroom;
    ordered.append(&mut near_threshold);

    if ordered.is_empty() {
        // Every admissible candidate is warned (or there are none). Fall back to
        // the warned admissible peers so a degraded request can still be
        // attempted; refused peers stay excluded.
        ordered = warned;
    }

    RankedCandidates { ordered, bands }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use vertex_swarm_api::{Au, Ledger};

    use super::*;

    fn peer(n: u8) -> OverlayAddress {
        OverlayAddress::from([n; 32])
    }

    struct FixedScores(HashMap<OverlayAddress, f64>);

    impl PeerScores for FixedScores {
        fn peer_score(&self, overlay: &OverlayAddress) -> Option<f64> {
            self.0.get(overlay).copied()
        }
    }

    /// A ledger that bands per peer at the unit price (`UnitPricer`): listed
    /// `unaffordable` peers refuse, listed `settle_due` peers settle-and-admit,
    /// everyone else admits.
    struct FixedLedger {
        unaffordable: Vec<OverlayAddress>,
        settle_due: Vec<OverlayAddress>,
    }

    impl FixedLedger {
        fn new(unaffordable: Vec<OverlayAddress>) -> Self {
            Self {
                unaffordable,
                settle_due: Vec::new(),
            }
        }
    }

    impl Ledger for FixedLedger {
        fn balance(&self, _overlay: &OverlayAddress) -> Au {
            Au::ZERO
        }

        fn reserved(&self, _overlay: &OverlayAddress) -> Au {
            Au::ZERO
        }

        fn disconnect_line(&self, overlay: &OverlayAddress) -> Au {
            // Unit price is 1: a zero line refuses, a wide line admits.
            if self.unaffordable.contains(overlay) {
                Au::ZERO
            } else {
                Au::from_amount(1000)
            }
        }

        fn settle_trigger(&self, overlay: &OverlayAddress) -> Au {
            // A zero trigger settles at the unit price; a wide one never settles.
            if self.settle_due.contains(overlay) {
                Au::ZERO
            } else {
                Au::from_amount(1000)
            }
        }
    }

    struct UnitPricer;

    impl SwarmPricing for UnitPricer {
        fn price(&self, _chunk: &ChunkAddress) -> Au {
            Au::from_amount(1)
        }

        fn peer_price(&self, _peer: &OverlayAddress, _chunk: &ChunkAddress) -> Au {
            Au::from_amount(1)
        }
    }

    #[derive(Default)]
    struct RecordingSettlement {
        triggered: Mutex<Vec<OverlayAddress>>,
    }

    impl SettlementTrigger for RecordingSettlement {
        fn trigger_settlement(&self, peer: OverlayAddress) {
            self.triggered.lock().unwrap().push(peer);
        }

        /// Records triggers but tracks no in-flight settle, so it reports `false`
        /// (nothing awaited), modelling the no-progress case.
        fn settled(
            &self,
            _peers: &[OverlayAddress],
        ) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
            Box::pin(std::future::ready(false))
        }
    }

    fn warned(peers: &[OverlayAddress]) -> impl Fn(&OverlayAddress) -> Option<f64> + '_ {
        move |p| {
            if peers.contains(p) {
                Some(DEFAULT_PEER_WARN_THRESHOLD)
            } else {
                Some(0.0)
            }
        }
    }

    /// Listed peers refuse at the disconnect line; everyone else admits.
    fn refusing(peers: &[OverlayAddress]) -> impl Fn(&OverlayAddress) -> Admission + '_ {
        move |p| {
            if peers.contains(p) {
                Admission::Refuse
            } else {
                Admission::Admit
            }
        }
    }

    /// Listed peers are past the settle trigger (`SettleAndAdmit`); everyone else
    /// has full forgiveness headroom (`Admit`).
    fn settle_due(peers: &[OverlayAddress]) -> impl Fn(&OverlayAddress) -> Admission + '_ {
        move |p| {
            if peers.contains(p) {
                Admission::SettleAndAdmit
            } else {
                Admission::Admit
            }
        }
    }

    #[test]
    fn spread_debt_tiering_leads_with_headroom_peers() {
        // peer(1) and peer(3) are past the settle trigger; under SpreadDebt the
        // headroom peers lead, proximity within each tier.
        let candidates = vec![peer(1), peer(2), peer(3), peer(4)];
        let ranked = rank_candidates(
            &candidates,
            warned(&[]),
            settle_due(&[peer(1), peer(3)]),
            Tiering::SpreadDebt,
        );
        assert_eq!(ranked.ordered, vec![peer(2), peer(4), peer(1), peer(3)]);
    }

    #[test]
    fn proximity_tiering_keeps_the_closest_admissible_peer_first() {
        // The same banding under Proximity ignores the headroom split: the closest
        // admissible peer leads even though it is past the settle trigger, so the
        // spill travels the minimum distance rather than skipping a near in-band
        // peer for a far headroom one.
        let candidates = vec![peer(1), peer(2), peer(3), peer(4)];
        let ranked = rank_candidates(
            &candidates,
            warned(&[]),
            settle_due(&[peer(1), peer(3)]),
            Tiering::Proximity,
        );
        assert_eq!(ranked.ordered, candidates);
    }

    #[test]
    fn proximity_tiering_still_drops_refused_and_warned_peers() {
        // Proximity changes only the admissible ordering: a refused peer is still
        // hard-skipped and a warned peer still excluded.
        let candidates = vec![peer(1), peer(2), peer(3)];
        let ranked = rank_candidates(
            &candidates,
            warned(&[peer(3)]),
            refusing(&[peer(1)]),
            Tiering::Proximity,
        );
        assert_eq!(ranked.ordered, vec![peer(2)]);
    }

    #[test]
    fn healthy_admitted_candidates_keep_proximity_order() {
        let candidates = vec![peer(1), peer(2), peer(3)];
        let ranked = rank_candidates(&candidates, warned(&[]), refusing(&[]), Tiering::SpreadDebt);
        assert_eq!(ranked.ordered, candidates);
    }

    #[test]
    fn warned_peer_is_excluded() {
        let candidates = vec![peer(1), peer(2), peer(3)];
        let ranked = rank_candidates(
            &candidates,
            warned(&[peer(2)]),
            refusing(&[]),
            Tiering::SpreadDebt,
        );
        assert_eq!(ranked.ordered, vec![peer(1), peer(3)]);
    }

    #[test]
    fn unknown_peer_is_not_treated_as_warned() {
        let candidates = vec![peer(1), peer(2)];
        let ranked = rank_candidates(&candidates, |_| None, refusing(&[]), Tiering::SpreadDebt);
        assert_eq!(ranked.ordered, candidates);
    }

    #[test]
    fn refused_peer_is_hard_skipped() {
        // A refused peer is excluded entirely, not deprioritized: sending would
        // cross its disconnect line.
        let candidates = vec![peer(1), peer(2), peer(3)];
        let ranked = rank_candidates(
            &candidates,
            warned(&[]),
            refusing(&[peer(1)]),
            Tiering::SpreadDebt,
        );
        assert_eq!(ranked.ordered, vec![peer(2), peer(3)]);
    }

    #[test]
    fn all_refused_yields_no_candidates() {
        // Every candidate would cross its disconnect line, so none is sendable.
        let candidates = vec![peer(1), peer(2), peer(3)];
        let ranked = rank_candidates(
            &candidates,
            warned(&[]),
            refusing(&[peer(1), peer(2), peer(3)]),
            Tiering::SpreadDebt,
        );
        assert!(ranked.ordered.is_empty());
    }

    #[test]
    fn all_warned_falls_back_to_proximity_order() {
        let candidates = vec![peer(1), peer(2)];
        let ranked = rank_candidates(
            &candidates,
            warned(&[peer(1), peer(2)]),
            refusing(&[]),
            Tiering::SpreadDebt,
        );
        assert_eq!(ranked.ordered, candidates);
    }

    #[test]
    fn refused_peer_is_not_resurrected_by_the_warned_fallback() {
        // peer(1) is refused and peer(2) is warned: the fallback returns the
        // warned admissible peer, never the refused one.
        let candidates = vec![peer(1), peer(2)];
        let ranked = rank_candidates(
            &candidates,
            warned(&[peer(2)]),
            refusing(&[peer(1)]),
            Tiering::SpreadDebt,
        );
        assert_eq!(ranked.ordered, vec![peer(2)]);
    }

    #[test]
    fn empty_candidates_stay_empty() {
        let ranked = rank_candidates(&[], warned(&[]), refusing(&[]), Tiering::SpreadDebt);
        assert!(ranked.ordered.is_empty());
    }

    fn selector(
        scores: HashMap<OverlayAddress, f64>,
        unaffordable: Vec<OverlayAddress>,
        settlement: Arc<RecordingSettlement>,
    ) -> PeerSelector {
        PeerSelector::new(
            Arc::new(FixedScores(scores)),
            Arc::new(FixedLedger::new(unaffordable)),
            Arc::new(UnitPricer),
            settlement,
        )
    }

    fn selector_with_settle_due(
        unaffordable: Vec<OverlayAddress>,
        settle_due: Vec<OverlayAddress>,
        settlement: Arc<RecordingSettlement>,
    ) -> PeerSelector {
        PeerSelector::new(
            Arc::new(FixedScores(HashMap::new())),
            Arc::new(FixedLedger {
                unaffordable,
                settle_due,
            }),
            Arc::new(UnitPricer),
            settlement,
        )
    }

    #[test]
    fn selector_hard_skips_a_refused_peer_and_settles_it() {
        // peer(1) is refused at the disconnect line: it is excluded from the
        // ordered list (retrieval routes to peer(2)) and a settle is triggered so
        // it drains back under its line.
        let settlement = Arc::new(RecordingSettlement::default());
        let sel = selector(HashMap::new(), vec![peer(1)], Arc::clone(&settlement));

        let ordered = sel.order(vec![peer(1), peer(2)], &ChunkAddress::zero());
        assert_eq!(ordered, vec![peer(2)], "the refused peer is hard-skipped");
        assert_eq!(
            *settlement.triggered.lock().unwrap(),
            vec![peer(1)],
            "the refused peer is settled so it drains"
        );
    }

    #[test]
    fn selector_settles_every_refused_candidate() {
        let settlement = Arc::new(RecordingSettlement::default());
        let sel = selector(
            HashMap::new(),
            vec![peer(1), peer(2)],
            Arc::clone(&settlement),
        );

        let ordered = sel.order(vec![peer(1), peer(2)], &ChunkAddress::zero());
        assert!(ordered.is_empty(), "all refused, nothing sendable");
        assert_eq!(
            *settlement.triggered.lock().unwrap(),
            vec![peer(1), peer(2)]
        );
    }

    #[test]
    fn selector_settles_proactively_when_debt_reaches_early_trigger() {
        // A still-affordable peer whose debt has reached the early-payment trigger
        // is settled before it becomes unaffordable, so its view of our debt drops
        // before it would refuse or drop us. The closer peer(1) is past the trigger
        // (SettleAndAdmit), so the headroom peer(2) leads it: debt spreads to a
        // peer with headroom before the near-threshold one is asked again.
        let settlement = Arc::new(RecordingSettlement::default());
        let sel = selector_with_settle_due(Vec::new(), vec![peer(1)], Arc::clone(&settlement));

        let ordered = sel.order(vec![peer(1), peer(2)], &ChunkAddress::zero());
        assert_eq!(ordered, vec![peer(2), peer(1)]);
        assert_eq!(*settlement.triggered.lock().unwrap(), vec![peer(1)]);
    }

    #[test]
    fn headroom_peers_lead_near_threshold_peers() {
        // peer(1) and peer(3) are past the settle trigger (SettleAndAdmit); peer(2)
        // and peer(4) still have forgiveness headroom (Admit). The headroom tier
        // leads, each tier in proximity order, so a bulk download spreads debt
        // across headroom peers before leaning on a near-threshold one.
        let settlement = Arc::new(RecordingSettlement::default());
        let sel =
            selector_with_settle_due(Vec::new(), vec![peer(1), peer(3)], Arc::clone(&settlement));

        let ordered = sel.order(
            vec![peer(1), peer(2), peer(3), peer(4)],
            &ChunkAddress::zero(),
        );
        assert_eq!(
            ordered,
            vec![peer(2), peer(4), peer(1), peer(3)],
            "headroom peers lead, proximity within each tier"
        );
    }

    #[test]
    fn selector_keeps_a_settle_and_admit_peer_but_skips_a_refused_one() {
        // Pushsync closeness: a peer in the tolerance band stays in the ordered
        // list (and is settled in parallel), while only a peer refused at the
        // disconnect line is dropped. peer(1) is in the band, peer(2) refuses.
        let settlement = Arc::new(RecordingSettlement::default());
        let sel = selector_with_settle_due(vec![peer(2)], vec![peer(1)], Arc::clone(&settlement));

        let ordered = sel.order(vec![peer(1), peer(2)], &ChunkAddress::zero());
        assert_eq!(
            ordered,
            vec![peer(1)],
            "the band peer is kept (closeness preserved), the refused peer skipped"
        );
        assert_eq!(
            *settlement.triggered.lock().unwrap(),
            vec![peer(1), peer(2)],
            "both the band peer and the refused peer settle"
        );
    }

    #[test]
    fn selector_does_not_settle_below_the_early_trigger() {
        // No candidate is over the trigger and all are affordable: no settle.
        let settlement = Arc::new(RecordingSettlement::default());
        let sel = selector_with_settle_due(Vec::new(), Vec::new(), Arc::clone(&settlement));

        let ordered = sel.order(vec![peer(1), peer(2)], &ChunkAddress::zero());
        assert_eq!(ordered, vec![peer(1), peer(2)]);
        assert!(settlement.triggered.lock().unwrap().is_empty());
    }

    #[test]
    fn selector_excludes_warned_peers() {
        let settlement = Arc::new(RecordingSettlement::default());
        let mut scores = HashMap::new();
        scores.insert(peer(1), DEFAULT_PEER_WARN_THRESHOLD - 1.0);
        let sel = selector(scores, Vec::new(), Arc::clone(&settlement));

        let ordered = sel.order(vec![peer(1), peer(2)], &ChunkAddress::zero());
        assert_eq!(ordered, vec![peer(2)]);
    }

    /// A ledger gated by a shared flag: while gated every peer refuses; once the
    /// flag clears every peer admits. Models a settle draining a peer's debt.
    struct FlipLedger(Arc<std::sync::atomic::AtomicBool>);

    impl Ledger for FlipLedger {
        fn balance(&self, _overlay: &OverlayAddress) -> Au {
            Au::ZERO
        }
        fn reserved(&self, _overlay: &OverlayAddress) -> Au {
            Au::ZERO
        }
        fn disconnect_line(&self, _overlay: &OverlayAddress) -> Au {
            if self.0.load(std::sync::atomic::Ordering::SeqCst) {
                Au::ZERO
            } else {
                Au::from_amount(1000)
            }
        }
        fn settle_trigger(&self, _overlay: &OverlayAddress) -> Au {
            Au::from_amount(1000)
        }
    }

    /// A settlement whose `settled` clears the shared gate and resolves at once,
    /// modelling a completed settle that drains the peer's debt.
    struct FlipSettlement(Arc<std::sync::atomic::AtomicBool>);

    impl SettlementTrigger for FlipSettlement {
        fn trigger_settlement(&self, _peer: OverlayAddress) {}
        fn settled(
            &self,
            _peers: &[OverlayAddress],
        ) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
            // Models an in-flight settle that completes and drains the debt:
            // reports `true` (a real completion was awaited).
            self.0.store(false, std::sync::atomic::Ordering::SeqCst);
            Box::pin(std::future::ready(true))
        }
    }

    /// A ledger that always refuses every peer at the unit price, modelling a
    /// close set that stays fully gated for the whole wait.
    struct AlwaysGatedLedger;

    impl Ledger for AlwaysGatedLedger {
        fn balance(&self, _overlay: &OverlayAddress) -> Au {
            Au::ZERO
        }
        fn reserved(&self, _overlay: &OverlayAddress) -> Au {
            Au::ZERO
        }
        fn disconnect_line(&self, _overlay: &OverlayAddress) -> Au {
            Au::ZERO
        }
        fn settle_trigger(&self, _overlay: &OverlayAddress) -> Au {
            Au::from_amount(1000)
        }
    }

    /// A settlement whose `settled` reports an in-flight completion that never
    /// drains debt, RTT-paced by a short sleep so the wait loop cannot busy-spin.
    struct StallSettlement;

    impl SettlementTrigger for StallSettlement {
        fn trigger_settlement(&self, _peer: OverlayAddress) {}
        fn settled(
            &self,
            _peers: &[OverlayAddress],
        ) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
            Box::pin(async {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                true
            })
        }
    }

    #[tokio::test]
    async fn order_or_wait_returns_a_peer_once_a_settle_makes_it_admissible() {
        // Every peer is gated on the first order; the awaited settle drains one
        // (the flag flips) and the re-order returns the now-admissible peers.
        let gated = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let sel = PeerSelector::new(
            Arc::new(FixedScores(HashMap::new())),
            Arc::new(FlipLedger(Arc::clone(&gated))),
            Arc::new(UnitPricer),
            Arc::new(FlipSettlement(Arc::clone(&gated))),
        );

        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        let ordered = sel
            .order_or_wait(vec![peer(1), peer(2)], &ChunkAddress::zero(), deadline)
            .await;
        assert_eq!(ordered, vec![peer(1), peer(2)], "recovers after the settle");
    }

    #[tokio::test]
    async fn order_or_wait_returns_empty_at_the_deadline_when_settles_never_drain() {
        // Every order stays empty and each awaited (in-flight) settle resolves
        // without draining debt: the loop runs to `deadline` and then returns
        // empty, never hanging, so the caller raises its generic transient
        // failure. A short deadline keeps the test fast.
        let sel = PeerSelector::new(
            Arc::new(FixedScores(HashMap::new())),
            Arc::new(AlwaysGatedLedger),
            Arc::new(UnitPricer),
            Arc::new(StallSettlement),
        );

        let wait = std::time::Duration::from_millis(120);
        let started = Instant::now();
        let deadline = started + wait;
        let ordered = sel
            .order_or_wait(vec![peer(1), peer(2)], &ChunkAddress::zero(), deadline)
            .await;
        let elapsed = started.elapsed();
        assert!(ordered.is_empty(), "still gated at the deadline");
        assert!(
            elapsed >= wait,
            "the in-flight wait runs to the deadline, not short of it"
        );
        assert!(
            elapsed < wait + std::time::Duration::from_secs(2),
            "the wait terminates at the deadline and does not hang"
        );
    }

    #[tokio::test]
    async fn order_or_wait_returns_empty_promptly_when_no_settle_is_in_flight() {
        // Every order stays empty and no settle is in flight to drain debt
        // (`settled` reports `false`): no re-order can ever admit a peer, so the
        // loop returns at once rather than spinning to a far-off deadline.
        let settlement = Arc::new(RecordingSettlement::default());
        let sel = selector(
            HashMap::new(),
            vec![peer(1), peer(2)],
            Arc::clone(&settlement),
        );

        let started = Instant::now();
        let deadline = started + std::time::Duration::from_secs(30);
        let ordered = sel
            .order_or_wait(vec![peer(1), peer(2)], &ChunkAddress::zero(), deadline)
            .await;
        assert!(ordered.is_empty(), "no peer is admissible");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "the no-progress guard returns promptly, well before the deadline"
        );
    }

    #[tokio::test]
    async fn order_or_wait_returns_the_first_order_without_awaiting_a_settle() {
        // An admissible close set returns on the first order: no settle is
        // triggered and the settle wait is never entered.
        let settlement = Arc::new(RecordingSettlement::default());
        let sel = selector(HashMap::new(), Vec::new(), Arc::clone(&settlement));

        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        let ordered = sel
            .order_or_wait(vec![peer(1), peer(2)], &ChunkAddress::zero(), deadline)
            .await;
        assert_eq!(ordered, vec![peer(1), peer(2)], "fast path");
        assert!(
            settlement.triggered.lock().unwrap().is_empty(),
            "no settle on the fast path"
        );
    }

    // Dedup of `AccountingSettlement` over a mock bandwidth accounting whose
    // settle parks until released, so two triggers for one peer can overlap.
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::Notify;
    use vertex_swarm_accounting::{NoProvideAction, NoReceiveAction};
    use vertex_swarm_api::{Direction, SwarmBandwidthAccounting, SwarmPeerBandwidth, SwarmResult};
    use vertex_swarm_test_utils::MockIdentity;
    use vertex_tasks::TaskManager;

    /// One process-wide multi-thread runtime for the settlement tests.
    ///
    /// `AccountingSettlement` spawns onto the global `TaskExecutor`, a process-wide
    /// `OnceLock` bound to the first `TaskManager::current()`. Binding it once to a
    /// long-lived runtime keeps every spawned settle on a live runtime regardless of
    /// test order; a per-test `#[tokio::test]` runtime would be torn down under the
    /// still-pointing global and starve the next test's settles.
    fn settlement_runtime() -> &'static tokio::runtime::Runtime {
        use std::sync::OnceLock;
        static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
        RUNTIME.get_or_init(|| {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("build settlement runtime");
            rt.block_on(async {
                // Bind the global executor to this runtime for the whole binary.
                std::mem::forget(TaskManager::current());
            });
            rt
        })
    }

    struct MockPeerBandwidth {
        peer: OverlayAddress,
        started: Arc<AtomicUsize>,
        finished: Arc<AtomicUsize>,
        gate: Arc<Notify>,
    }

    impl SwarmPeerBandwidth for MockPeerBandwidth {
        fn record(&self, _amount: Au, _direction: Direction) {}
        fn balance(&self) -> Au {
            Au::ZERO
        }
        async fn settle(&self) -> SwarmResult<()> {
            self.started.fetch_add(1, Ordering::SeqCst);
            self.gate.notified().await;
            self.finished.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn peer(&self) -> OverlayAddress {
            self.peer
        }
    }

    struct MockBandwidth {
        for_peer_calls: Arc<AtomicUsize>,
        started: Arc<AtomicUsize>,
        finished: Arc<AtomicUsize>,
        gate: Arc<Notify>,
    }

    impl SwarmBandwidthAccounting for MockBandwidth {
        type Identity = MockIdentity;
        type Peer = MockPeerBandwidth;
        type ReceiveAction = NoReceiveAction;
        type ProvideAction = NoProvideAction;

        fn identity(&self) -> &Self::Identity {
            unreachable!("settlement never reads the identity")
        }
        fn for_peer(&self, peer: OverlayAddress) -> Self::Peer {
            self.for_peer_calls.fetch_add(1, Ordering::SeqCst);
            MockPeerBandwidth {
                peer,
                started: Arc::clone(&self.started),
                finished: Arc::clone(&self.finished),
                gate: Arc::clone(&self.gate),
            }
        }
        fn peers(&self) -> Vec<OverlayAddress> {
            Vec::new()
        }
        fn remove_peer(&self, _peer: &OverlayAddress) {}
        fn prepare_receive(
            &self,
            _peer: OverlayAddress,
            _price: Au,
            _originated: bool,
        ) -> SwarmResult<Self::ReceiveAction> {
            Ok(NoReceiveAction)
        }
        fn prepare_provide(
            &self,
            _peer: OverlayAddress,
            _price: Au,
        ) -> SwarmResult<Self::ProvideAction> {
            Ok(NoProvideAction)
        }
    }

    #[test]
    fn trigger_settlement_runs_one_settle_per_peer_in_flight() {
        // The trigger spawns its settle on the shared global task executor.
        settlement_runtime().block_on(async {
            let for_peer_calls = Arc::new(AtomicUsize::new(0));
            let started = Arc::new(AtomicUsize::new(0));
            let finished = Arc::new(AtomicUsize::new(0));
            let gate = Arc::new(Notify::new());
            let bandwidth = Arc::new(MockBandwidth {
                for_peer_calls: Arc::clone(&for_peer_calls),
                started: Arc::clone(&started),
                finished: Arc::clone(&finished),
                gate: Arc::clone(&gate),
            });
            let trigger = AccountingSettlement::new(bandwidth);

            // Two synchronous triggers for one peer. The first reserves the peer and
            // spawns a settle that parks on the gate; the second finds the peer
            // already in flight and is dropped before it reaches the accounting, so
            // only one settle is ever created.
            trigger.trigger_settlement(peer(1));
            trigger.trigger_settlement(peer(1));
            assert_eq!(
                for_peer_calls.load(Ordering::SeqCst),
                1,
                "the second trigger is deduped while the first settle is in flight"
            );

            // Release the parked settle and wait for it to finish; the peer then
            // leaves the in-flight set. `notify_one` stores a permit if the settle has
            // not parked yet, so the wakeup is never lost.
            gate.notify_one();
            while finished.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }

            // Once the prior settle has cleared the set, a fresh trigger starts a new
            // settle. Retry across the brief window in which the spawned wrapper has
            // not yet removed the peer.
            let mut tries = 0;
            while for_peer_calls.load(Ordering::SeqCst) < 2 {
                trigger.trigger_settlement(peer(1));
                tries += 1;
                assert!(
                    tries < 10_000,
                    "in-flight set never cleared after completion"
                );
                tokio::task::yield_now().await;
            }
            gate.notify_one();
        });
    }

    #[test]
    fn settled_resolves_when_the_in_flight_settle_completes() {
        // A waiter on a peer's in-flight settle resolves once that settle acks,
        // which is what paces the gated-set wait loop.
        settlement_runtime().block_on(async {
            let for_peer_calls = Arc::new(AtomicUsize::new(0));
            let started = Arc::new(AtomicUsize::new(0));
            let finished = Arc::new(AtomicUsize::new(0));
            let gate = Arc::new(Notify::new());
            let bandwidth = Arc::new(MockBandwidth {
                for_peer_calls,
                started: Arc::clone(&started),
                finished,
                gate: Arc::clone(&gate),
            });
            let trigger = AccountingSettlement::new(bandwidth);

            // No settle in flight: the waiter resolves immediately.
            trigger.settled(&[peer(1)]).await;

            // Start a settle that parks on the gate, then take a waiter on it.
            trigger.trigger_settlement(peer(1));
            while started.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
            let waiter = trigger.settled(&[peer(1)]);

            // Release the parked settle; the guard fires the completion on drop,
            // so the waiter resolves. A bounded timeout proves it does not hang.
            gate.notify_one();
            vertex_tasks::time::timeout(std::time::Duration::from_secs(5), waiter)
                .await
                .expect("settled resolves once the in-flight settle completes");
        });
    }

    #[test]
    fn in_flight_guard_clears_the_peer_on_drop() {
        // The guard removes the peer in Drop, which runs on normal completion,
        // unwind (a panicking settle), and cancellation alike, so a settle that
        // never completes cleanly cannot pin the peer and starve its settlement.
        let in_flight = Arc::new(parking_lot::Mutex::new(InFlightMap::default()));
        let (done, completion) = oneshot::channel();
        in_flight.lock().insert(peer(1), completion.shared());
        {
            let _guard = InFlightGuard {
                in_flight: Arc::clone(&in_flight),
                peer: peer(1),
                done: Some(done),
            };
            assert!(in_flight.lock().contains_key(&peer(1)));
        }
        assert!(
            !in_flight.lock().contains_key(&peer(1)),
            "the guard must clear the peer when dropped"
        );
    }
}
