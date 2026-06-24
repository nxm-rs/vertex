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

use std::sync::Arc;

use nectar_primitives::ChunkAddress;
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
pub struct AccountingSettlement<B> {
    bandwidth: B,
}

impl<B> AccountingSettlement<B> {
    /// Trigger settlement through `bandwidth`.
    pub fn new(bandwidth: B) -> Self {
        Self { bandwidth }
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
        let handle = self.bandwidth.for_peer(peer);
        executor.spawn(async move {
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
    /// debtor-initiated settlement for any candidate whose real debt has reached
    /// the early-payment trigger, so a peer's view of our debt is reduced before
    /// it reaches the peer's threshold and it refuses or drops us. Settlement is
    /// also triggered for the unaffordable candidates when the request is blocked
    /// on balance (none is affordable), so even a debt that built without
    /// crossing the early trigger is still settled before we give up on the peer.
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

        // Proactively settle any candidate whose debt has reached the early
        // trigger. The settlement service rate-limits per peer, so polling this
        // every request is cheap: a settle is sent at most once per refresh
        // interval and skipped while one is in flight.
        for peer in &candidates {
            if self.affordability.should_settle(peer) {
                self.settlement.trigger_settlement(*peer);
            }
        }

        // When the request is blocked on balance every unaffordable candidate is
        // settled regardless of the early trigger: the next request must find a
        // usable peer.
        if ranked.blocked_on_balance() {
            for peer in &ranked.unaffordable {
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
        // A still-affordable peer whose debt has reached the early-payment
        // trigger is settled before it becomes unaffordable, so its view of our
        // debt is reduced before it would refuse or drop us.
        let settlement = Arc::new(RecordingSettlement::default());
        let sel = selector_with_settle_due(Vec::new(), vec![peer(1)], Arc::clone(&settlement));

        let ordered = sel.order(vec![peer(1), peer(2)], &ChunkAddress::zero());
        // Both stay affordable and keep proximity order.
        assert_eq!(ordered, vec![peer(1), peer(2)]);
        // Only the over-trigger peer is settled.
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
}
