//! Score- and affordability-aware peer selection for retrieval and pushsync.
//!
//! Topology returns candidate storers in proximity order. [`PeerSelector`]
//! reorders them with two additional signals before a request goes out:
//!
//! - Peers whose score is in the warned range are excluded, so a peer that is
//!   being scored down is not asked again while it misbehaves.
//! - Peers we cannot afford (the per-chunk debit would cross their disconnect
//!   threshold) rank behind affordable ones.
//!
//! Proximity stays the primary key within each group. When every candidate is
//! warned or unaffordable, the original proximity order is used unchanged:
//! degraded service beats failing the request outright. When a request is
//! blocked on balance (no affordable candidate at all), a best-effort
//! settlement is triggered for the unaffordable candidates so they become
//! usable again; the settlement providers themselves decide whether any
//! payment is actually due.
//!
//! The price consulted per candidate is [`SwarmPricing::peer_price`], the same
//! per-peer chunk price the accounting layer debits when the request is
//! served.

use std::collections::HashSet;
use std::sync::Arc;

use nectar_primitives::ChunkAddress;
use parking_lot::Mutex;
use tracing::debug;
use vertex_swarm_api::{
    DEFAULT_PEER_WARN_THRESHOLD, PeerAffordability, SwarmBandwidthAccounting, SwarmIdentity,
    SwarmPeerBandwidth, SwarmPricing,
};
use vertex_swarm_primitives::OverlayAddress;
use vertex_swarm_topology::TopologyHandle;
use vertex_tasks::TaskExecutor;

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

/// Best-effort settlement trigger for requests blocked on balance.
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
/// start until the prior one is acked).
pub struct AccountingSettlement<B> {
    bandwidth: B,
    in_flight: Arc<Mutex<HashSet<OverlayAddress>>>,
}

impl<B> AccountingSettlement<B> {
    /// Trigger settlement through `bandwidth`.
    pub fn new(bandwidth: B) -> Self {
        Self {
            bandwidth,
            in_flight: Arc::new(Mutex::new(HashSet::new())),
        }
    }
}

/// Removes a peer from the in-flight set on drop, so a panic or cancellation of
/// the settle future cannot pin the peer and starve its settlement.
struct InFlightGuard {
    in_flight: Arc<Mutex<HashSet<OverlayAddress>>>,
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
        if !self.in_flight.lock().insert(peer) {
            return;
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

/// Reorders proximity-ordered candidates by score and affordability.
///
/// Built by the node assembly from the topology handle (scores), bandwidth
/// accounting (affordability and settlement), and the pricer (per-peer chunk
/// price). Consumed by the retrieval and pushsync candidate-selection paths.
pub struct PeerSelector {
    scores: Arc<dyn PeerScores>,
    affordability: Arc<dyn PeerAffordability>,
    pricing: Arc<dyn SwarmPricing>,
    settlement: Arc<dyn SettlementTrigger>,
}

impl PeerSelector {
    /// Compose a selector from its query and trigger surfaces.
    pub fn new(
        scores: Arc<dyn PeerScores>,
        affordability: Arc<dyn PeerAffordability>,
        pricing: Arc<dyn SwarmPricing>,
        settlement: Arc<dyn SettlementTrigger>,
    ) -> Self {
        Self {
            scores,
            affordability,
            pricing,
            settlement,
        }
    }

    /// Order `candidates` (in proximity order) for a request on `chunk`.
    ///
    /// Applies the ranking described at the module level. Triggers best-effort
    /// settlement for any candidate whose debt has reached the early-payment
    /// trigger, so the peer's view of our debt drops before it reaches the
    /// disconnect threshold. When the request is blocked on balance (candidates
    /// exist but none is affordable), the unaffordable candidates are settled too,
    /// so a debt that built without crossing the early trigger is still settled
    /// before the request gives up on the peer. A single pass triggers each peer
    /// at most once; the in-flight set dedups across repeated calls.
    pub fn order(
        &self,
        candidates: Vec<OverlayAddress>,
        chunk: &ChunkAddress,
    ) -> Vec<OverlayAddress> {
        let ranked = rank_candidates(
            &candidates,
            |peer| self.scores.peer_score(peer),
            |peer| {
                self.affordability
                    .can_afford(peer, self.pricing.peer_price(peer, chunk))
            },
        );

        let blocked = ranked.blocked_on_balance();
        for peer in &candidates {
            if self.affordability.should_settle(peer)
                || (blocked && ranked.unaffordable.contains(peer))
            {
                self.settlement.trigger_settlement(*peer);
            }
        }

        ranked.ordered
    }
}

/// Outcome of ranking a candidate set.
struct RankedCandidates {
    /// Candidates to attempt, best first.
    ordered: Vec<OverlayAddress>,
    /// How many of `ordered` passed the affordability check.
    affordable: usize,
    /// Candidates that failed the affordability check, in proximity order.
    unaffordable: Vec<OverlayAddress>,
}

impl RankedCandidates {
    /// True when candidates exist but none is currently affordable.
    fn blocked_on_balance(&self) -> bool {
        self.affordable == 0 && !self.unaffordable.is_empty()
    }
}

/// Rank proximity-ordered `candidates` by score and affordability.
///
/// Warned peers (score at or below [`DEFAULT_PEER_WARN_THRESHOLD`]) are
/// excluded. Affordable peers keep their proximity order and rank before
/// unaffordable ones, which also keep theirs. If every candidate is warned,
/// the original proximity order is returned unchanged.
fn rank_candidates(
    candidates: &[OverlayAddress],
    score: impl Fn(&OverlayAddress) -> Option<f64>,
    can_afford: impl Fn(&OverlayAddress) -> bool,
) -> RankedCandidates {
    let mut ordered = Vec::with_capacity(candidates.len());
    let mut unaffordable = Vec::new();

    for peer in candidates {
        if score(peer).is_some_and(|s| s <= DEFAULT_PEER_WARN_THRESHOLD) {
            continue;
        }
        if can_afford(peer) {
            ordered.push(*peer);
        } else {
            unaffordable.push(*peer);
        }
    }

    if ordered.is_empty() && unaffordable.is_empty() {
        // Every candidate is warned. Fall back to plain proximity order so a
        // degraded request can still be attempted.
        return RankedCandidates {
            ordered: candidates.to_vec(),
            affordable: 0,
            unaffordable: Vec::new(),
        };
    }

    let affordable = ordered.len();
    ordered.extend_from_slice(&unaffordable);
    RankedCandidates {
        ordered,
        affordable,
        unaffordable,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use vertex_swarm_api::Au;

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

    struct FixedAffordability {
        unaffordable: Vec<OverlayAddress>,
        /// Peers whose debt has reached the early-payment trigger.
        settle_due: Vec<OverlayAddress>,
    }

    impl FixedAffordability {
        fn new(unaffordable: Vec<OverlayAddress>) -> Self {
            Self {
                unaffordable,
                settle_due: Vec::new(),
            }
        }
    }

    impl PeerAffordability for FixedAffordability {
        fn can_afford(&self, overlay: &OverlayAddress, _price: Au) -> bool {
            !self.unaffordable.contains(overlay)
        }

        fn allowance_remaining(&self, _overlay: &OverlayAddress) -> Au {
            Au::ZERO
        }

        fn should_settle(&self, overlay: &OverlayAddress) -> bool {
            self.settle_due.contains(overlay)
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

    fn unaffordable(peers: &[OverlayAddress]) -> impl Fn(&OverlayAddress) -> bool + '_ {
        move |p| !peers.contains(p)
    }

    #[test]
    fn healthy_affordable_candidates_keep_proximity_order() {
        let candidates = vec![peer(1), peer(2), peer(3)];
        let ranked = rank_candidates(&candidates, warned(&[]), unaffordable(&[]));
        assert_eq!(ranked.ordered, candidates);
        assert_eq!(ranked.affordable, 3);
        assert!(ranked.unaffordable.is_empty());
        assert!(!ranked.blocked_on_balance());
    }

    #[test]
    fn warned_peer_is_excluded() {
        let candidates = vec![peer(1), peer(2), peer(3)];
        let ranked = rank_candidates(&candidates, warned(&[peer(2)]), unaffordable(&[]));
        assert_eq!(ranked.ordered, vec![peer(1), peer(3)]);
    }

    #[test]
    fn unknown_peer_is_not_treated_as_warned() {
        let candidates = vec![peer(1), peer(2)];
        let ranked = rank_candidates(&candidates, |_| None, unaffordable(&[]));
        assert_eq!(ranked.ordered, candidates);
    }

    #[test]
    fn unaffordable_peer_is_deprioritized() {
        let candidates = vec![peer(1), peer(2), peer(3)];
        let ranked = rank_candidates(&candidates, warned(&[]), unaffordable(&[peer(1)]));
        assert_eq!(ranked.ordered, vec![peer(2), peer(3), peer(1)]);
        assert_eq!(ranked.affordable, 2);
        assert!(!ranked.blocked_on_balance());
    }

    #[test]
    fn all_unaffordable_falls_back_to_proximity_order() {
        let candidates = vec![peer(1), peer(2), peer(3)];
        let ranked = rank_candidates(
            &candidates,
            warned(&[]),
            unaffordable(&[peer(1), peer(2), peer(3)]),
        );
        assert_eq!(ranked.ordered, candidates);
        assert!(ranked.blocked_on_balance());
    }

    #[test]
    fn all_warned_falls_back_to_proximity_order() {
        let candidates = vec![peer(1), peer(2)];
        let ranked = rank_candidates(&candidates, warned(&[peer(1), peer(2)]), unaffordable(&[]));
        assert_eq!(ranked.ordered, candidates);
        assert!(!ranked.blocked_on_balance());
    }

    #[test]
    fn empty_candidates_stay_empty() {
        let ranked = rank_candidates(&[], warned(&[]), unaffordable(&[]));
        assert!(ranked.ordered.is_empty());
        assert!(!ranked.blocked_on_balance());
    }

    fn selector(
        scores: HashMap<OverlayAddress, f64>,
        unaffordable: Vec<OverlayAddress>,
        settlement: Arc<RecordingSettlement>,
    ) -> PeerSelector {
        PeerSelector::new(
            Arc::new(FixedScores(scores)),
            Arc::new(FixedAffordability::new(unaffordable)),
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
            Arc::new(FixedAffordability {
                unaffordable,
                settle_due,
            }),
            Arc::new(UnitPricer),
            settlement,
        )
    }

    #[test]
    fn selector_orders_and_skips_settlement_when_affordable_exists() {
        let settlement = Arc::new(RecordingSettlement::default());
        let sel = selector(HashMap::new(), vec![peer(1)], Arc::clone(&settlement));

        let ordered = sel.order(vec![peer(1), peer(2)], &ChunkAddress::zero());
        assert_eq!(ordered, vec![peer(2), peer(1)]);
        assert!(settlement.triggered.lock().unwrap().is_empty());
    }

    #[test]
    fn selector_triggers_settlement_when_blocked_on_balance() {
        let settlement = Arc::new(RecordingSettlement::default());
        let sel = selector(
            HashMap::new(),
            vec![peer(1), peer(2)],
            Arc::clone(&settlement),
        );

        let ordered = sel.order(vec![peer(1), peer(2)], &ChunkAddress::zero());
        assert_eq!(ordered, vec![peer(1), peer(2)]);
        assert_eq!(
            *settlement.triggered.lock().unwrap(),
            vec![peer(1), peer(2)]
        );
    }

    #[test]
    fn selector_settles_proactively_when_debt_reaches_early_trigger() {
        // A still-affordable peer whose debt has reached the early-payment trigger
        // is settled before it becomes unaffordable, so its view of our debt drops
        // before it would refuse or drop us.
        let settlement = Arc::new(RecordingSettlement::default());
        let sel = selector_with_settle_due(Vec::new(), vec![peer(1)], Arc::clone(&settlement));

        let ordered = sel.order(vec![peer(1), peer(2)], &ChunkAddress::zero());
        assert_eq!(ordered, vec![peer(1), peer(2)]);
        assert_eq!(*settlement.triggered.lock().unwrap(), vec![peer(1)]);
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
        fn allow(&self, _amount: Au) -> bool {
            true
        }
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
        let in_flight = Arc::new(parking_lot::Mutex::new(HashSet::new()));
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
