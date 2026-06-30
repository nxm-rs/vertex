//! Score- and credit-aware peer selection for retrieval and pushsync.
//!
//! Topology returns candidate storers in proximity order. [`PeerSelector`]
//! reorders them with three additional signals before a request goes out. The
//! reorder is a pure function: the selector never triggers settlement. A request
//! settles the one peer it actually dispatches to, at the origin credit gate, so
//! the settle fan-out is the legs contacted rather than the whole window.
//!
//! - A peer the admission band refuses (the per-chunk debit would cross its
//!   disconnect line) is hard-skipped, so the request routes to the next-closest
//!   affordable peer. The dispatch that crossed its line triggers one settle to
//!   draw that peer back under its line.
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
//! refused peer is never resurrected this way.
//!
//! [`Admit`]: Admission::Admit
//! [`SettleAndAdmit`]: Admission::SettleAndAdmit
//!
//! The price consulted per candidate is [`SwarmPricing::peer_price`], the same
//! per-peer chunk price the accounting layer debits when the request is
//! served.

use std::collections::HashSet;
use std::sync::Arc;

use nectar_primitives::ChunkAddress;
use parking_lot::Mutex;
use rustc_hash::FxBuildHasher;
use tracing::debug;
use vertex_swarm_api::{
    Admission, AdmissionControl, DEFAULT_PEER_WARN_THRESHOLD, SwarmBandwidthAccounting,
    SwarmIdentity, SwarmPeerBandwidth, SwarmPricing,
};
use vertex_swarm_primitives::OverlayAddress;
use vertex_swarm_topology::TopologyHandle;
use vertex_tasks::TaskExecutor;

/// Per-peer in-flight settle set: a peer is present while a settle to it is
/// running, so a second trigger for it is deduped (the per-peer rate limit, the
/// next settle cannot start until the prior one clears). Overlay keys are
/// uniformly random, so a fast non-DoS hasher keeps the dedup off SipHash.
type InFlightSet = HashSet<OverlayAddress, FxBuildHasher>;

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
}

/// [`SettlementTrigger`] over a bandwidth accounting instance.
///
/// Settlement runs as a spawned task on the current task executor so
/// selection never blocks on settlement I/O. Failures are logged at debug
/// level; the next request retries naturally.
///
/// A shared in-flight set keeps at most one settle per peer running at a time:
/// the second trigger for a peer with a settle still outstanding is dropped, so
/// the set is both the dedup and the per-peer rate limit (the next settle cannot
/// start until the prior one clears on completion).
pub struct AccountingSettlement<B> {
    bandwidth: B,
    in_flight: Arc<Mutex<InFlightSet>>,
}

impl<B> AccountingSettlement<B> {
    /// Trigger settlement through `bandwidth`.
    pub fn new(bandwidth: B) -> Self {
        Self {
            bandwidth,
            in_flight: Arc::new(Mutex::new(InFlightSet::default())),
        }
    }
}

/// Removes a peer from the in-flight set on drop, so a panic or cancellation of
/// the settle future cannot pin the peer (which would starve its settlement).
struct InFlightGuard {
    in_flight: Arc<Mutex<InFlightSet>>,
    peer: OverlayAddress,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.in_flight.lock().remove(&self.peer);
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
        // one.
        {
            let mut in_flight = self.in_flight.lock();
            if !in_flight.insert(peer) {
                return;
            }
        }
        let handle = self.bandwidth.for_peer(peer);
        let guard = InFlightGuard {
            in_flight: Arc::clone(&self.in_flight),
            peer,
        };
        executor.spawn(async move {
            let _guard = guard;
            if let Err(error) = handle.settle().await {
                debug!(%peer, %error, "best-effort settlement failed");
            }
        });
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
}

impl PeerSelector {
    /// Compose a selector from its query surfaces.
    ///
    /// Ranking only: the selector never triggers settlement. A request settles
    /// the peer it actually dispatches to, at the origin credit gate, so the
    /// fan-out is the legs contacted rather than the whole candidate window.
    pub fn new(
        scores: Arc<dyn PeerScores>,
        admission: Arc<dyn AdmissionControl>,
        pricing: Arc<dyn SwarmPricing>,
    ) -> Self {
        Self {
            scores,
            admission,
            pricing,
        }
    }

    /// Order `candidates` (in proximity order) for a request on `chunk`.
    ///
    /// Pure ranking, no side effect: a [`Refuse`] candidate is hard-skipped
    /// (sending would cross its disconnect line), warned peers are excluded, and
    /// the admissible rest are tiered headroom-first (an [`Admit`] peer leads a
    /// [`SettleAndAdmit`] one) with proximity the key within a tier. Settlement is
    /// triggered by the dispatch path for the peer a request actually contacts,
    /// not for the whole window here.
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
    /// one. Refuse and warned handling match [`order`](Self::order); like it, this
    /// is pure ranking and triggers no settlement.
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
        // Pure ranking: the band hard-skips a refused peer and tiers the rest, and
        // nothing is settled here. A request settles only the peer it actually
        // dispatches to, at the origin credit gate, so the settle fan-out is the
        // legs contacted rather than the whole candidate window.
        rank_candidates(
            &candidates,
            |peer| self.scores.peer_score(peer),
            |peer| {
                self.admission
                    .admit(peer, self.pricing.peer_price(peer, chunk))
            },
            tiering,
        )
    }
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
/// never resurrected this way. `admit` is invoked exactly once per candidate.
fn rank_candidates(
    candidates: &[OverlayAddress],
    score: impl Fn(&OverlayAddress) -> Option<f64>,
    admit: impl Fn(&OverlayAddress) -> Admission,
    tiering: Tiering,
) -> Vec<OverlayAddress> {
    let mut headroom = Vec::with_capacity(candidates.len());
    let mut near_threshold = Vec::new();
    let mut warned = Vec::new();

    for peer in candidates {
        let band = admit(peer);
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

    ordered
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

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
        assert_eq!(ranked, vec![peer(2), peer(4), peer(1), peer(3)]);
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
        assert_eq!(ranked, candidates);
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
        assert_eq!(ranked, vec![peer(2)]);
    }

    #[test]
    fn healthy_admitted_candidates_keep_proximity_order() {
        let candidates = vec![peer(1), peer(2), peer(3)];
        let ranked = rank_candidates(&candidates, warned(&[]), refusing(&[]), Tiering::SpreadDebt);
        assert_eq!(ranked, candidates);
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
        assert_eq!(ranked, vec![peer(1), peer(3)]);
    }

    #[test]
    fn unknown_peer_is_not_treated_as_warned() {
        let candidates = vec![peer(1), peer(2)];
        let ranked = rank_candidates(&candidates, |_| None, refusing(&[]), Tiering::SpreadDebt);
        assert_eq!(ranked, candidates);
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
        assert_eq!(ranked, vec![peer(2), peer(3)]);
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
        assert!(ranked.is_empty());
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
        assert_eq!(ranked, candidates);
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
        assert_eq!(ranked, vec![peer(2)]);
    }

    #[test]
    fn empty_candidates_stay_empty() {
        let ranked = rank_candidates(&[], warned(&[]), refusing(&[]), Tiering::SpreadDebt);
        assert!(ranked.is_empty());
    }

    fn selector(
        scores: HashMap<OverlayAddress, f64>,
        unaffordable: Vec<OverlayAddress>,
    ) -> PeerSelector {
        PeerSelector::new(
            Arc::new(FixedScores(scores)),
            Arc::new(FixedLedger::new(unaffordable)),
            Arc::new(UnitPricer),
        )
    }

    fn selector_with_settle_due(
        unaffordable: Vec<OverlayAddress>,
        settle_due: Vec<OverlayAddress>,
    ) -> PeerSelector {
        PeerSelector::new(
            Arc::new(FixedScores(HashMap::new())),
            Arc::new(FixedLedger {
                unaffordable,
                settle_due,
            }),
            Arc::new(UnitPricer),
        )
    }

    #[test]
    fn selector_hard_skips_a_refused_peer() {
        // peer(1) is refused at the disconnect line: it is excluded from the
        // ordered list, so retrieval routes to peer(2). Settling the peer a
        // request dispatches to is the credit gate's job, not the selector's.
        let sel = selector(HashMap::new(), vec![peer(1)]);

        let ordered = sel.order(vec![peer(1), peer(2)], &ChunkAddress::zero());
        assert_eq!(ordered, vec![peer(2)], "the refused peer is hard-skipped");
    }

    #[test]
    fn headroom_peers_lead_near_threshold_peers() {
        // peer(1) and peer(3) are past the settle trigger (SettleAndAdmit); peer(2)
        // and peer(4) still have forgiveness headroom (Admit). The headroom tier
        // leads, each tier in proximity order, so a bulk download spreads debt
        // across headroom peers before leaning on a near-threshold one.
        let sel = selector_with_settle_due(Vec::new(), vec![peer(1), peer(3)]);

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
    fn selector_keeps_a_band_peer_but_skips_a_refused_one() {
        // A peer in the tolerance band stays in the ordered list (closeness
        // preserved), while only a peer refused at the disconnect line is dropped.
        // peer(1) is in the band, peer(2) refuses.
        let sel = selector_with_settle_due(vec![peer(2)], vec![peer(1)]);

        let ordered = sel.order(vec![peer(1), peer(2)], &ChunkAddress::zero());
        assert_eq!(
            ordered,
            vec![peer(1)],
            "the band peer is kept (closeness preserved), the refused peer skipped"
        );
    }

    #[test]
    fn selector_excludes_warned_peers() {
        let mut scores = HashMap::new();
        scores.insert(peer(1), DEFAULT_PEER_WARN_THRESHOLD - 1.0);
        let sel = selector(scores, Vec::new());

        let ordered = sel.order(vec![peer(1), peer(2)], &ChunkAddress::zero());
        assert_eq!(ordered, vec![peer(2)]);
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
    fn in_flight_guard_clears_the_peer_on_drop() {
        // The guard removes the peer in Drop, which runs on normal completion,
        // unwind (a panicking settle), and cancellation alike, so a settle that
        // never completes cleanly cannot pin the peer and starve its settlement.
        let in_flight = Arc::new(parking_lot::Mutex::new(InFlightSet::default()));
        in_flight.lock().insert(peer(1));
        {
            let _guard = InFlightGuard {
                in_flight: Arc::clone(&in_flight),
                peer: peer(1),
            };
            assert!(in_flight.lock().contains(&peer(1)));
        }
        assert!(
            !in_flight.lock().contains(&peer(1)),
            "the guard must clear the peer when dropped"
        );
    }
}
