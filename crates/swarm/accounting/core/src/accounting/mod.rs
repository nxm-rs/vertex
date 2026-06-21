//! Per-peer bandwidth accounting.
//!
//! # Accounting Units (AU)
//!
//! All values are in **Accounting Units**, not bytes or BZZ tokens. AUs encode
//! network cost based on Kademlia proximity:
//!
//! ```text
//! price = (max_po - proximity + 1) × base_price
//! ```
//!
//! Closer chunks (higher proximity) cost less; distant chunks cost more.
//!
//! # Components
//!
//! - [`PeerState`] - Atomic per-peer balance counters
//! - [`Accounting`] - Factory with pluggable settlement providers
//! - [`ReceiveAction`] / [`ProvideAction`] - Prepare/apply pattern for balance changes

mod action;
mod error;
mod peer;

pub use action::{AccountingAction, ProvideAction, ReceiveAction};
pub use error::AccountingError;
pub use peer::{PeerState, PeerStateSnapshot};

use alloc::vec::Vec;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

use vertex_swarm_api::{
    Au, Direction, PeerAffordability, PeerReporter, ReportSource, SwarmAccountingConfig,
    SwarmBandwidthAccounting, SwarmIdentity, SwarmPeerBandwidth, SwarmResult, SwarmScoringEvent,
};
use vertex_swarm_primitives::OverlayAddress;

use vertex_swarm_api::SwarmSettlementProvider;

/// Per-peer accounting with pluggable settlement providers.
///
/// Manages balances and delegates settlement to configured providers.
/// Without providers, behaves as a simple balance tracker.
pub struct Accounting<C, I: SwarmIdentity> {
    config: C,
    identity: I,
    providers: Arc<[Box<dyn SwarmSettlementProvider>]>,
    peers: RwLock<HashMap<OverlayAddress, Arc<PeerState>>>,
    reporter: Option<Arc<dyn PeerReporter>>,
}

impl<C: SwarmAccountingConfig, I: SwarmIdentity> Accounting<C, I> {
    /// Create a new accounting instance with no settlement providers.
    pub fn new(config: C, identity: I) -> Self {
        Self {
            config,
            identity,
            providers: Arc::from(Vec::new()),
            peers: RwLock::new(HashMap::new()),
            reporter: None,
        }
    }

    /// Create a new accounting instance with the given settlement providers.
    ///
    /// Providers are called in order during settlement operations; pseudosettle
    /// should come before swap.
    pub fn with_providers(
        config: C,
        identity: I,
        providers: Vec<Box<dyn SwarmSettlementProvider>>,
    ) -> Self {
        Self {
            config,
            identity,
            providers: Arc::from(providers),
            peers: RwLock::new(HashMap::new()),
            reporter: None,
        }
    }

    /// Attach a peer reporter so accounting violations feed peer scoring.
    ///
    /// Reporting is best-effort and non-blocking. Without a reporter,
    /// violations only surface as errors to the caller, exactly as before.
    pub fn with_reporter(mut self, reporter: Arc<dyn PeerReporter>) -> Self {
        self.reporter = Some(reporter);
        self
    }

    /// Get a reference to the configuration.
    pub fn config(&self) -> &C {
        &self.config
    }

    /// Get a reference to the settlement providers.
    pub fn providers(&self) -> &[Box<dyn SwarmSettlementProvider>] {
        &self.providers
    }

    /// Returns the names of the active settlement providers.
    pub fn provider_names(&self) -> Vec<&str> {
        self.providers.iter().map(|p| p.name()).collect()
    }

    /// Prepare a receive action (we are receiving service, balance decreases).
    pub fn prepare_receive(
        &self,
        peer: OverlayAddress,
        price: Au,
        _originated: bool,
    ) -> Result<ReceiveAction, AccountingError> {
        let state = self.get_or_create_peer(peer);

        let current_balance = state.balance();
        let reserved = state.reserved_balance();
        let projected = current_balance
            .saturating_sub(price)
            .saturating_sub(reserved);

        let disconnect_threshold = self.config.disconnect_threshold();
        let threshold = -disconnect_threshold;
        if projected < threshold {
            // Law broken: the peer stopped accepting settlement (refresh or
            // payment), letting our debt reach the disconnect threshold.
            //
            // Reported once per breach episode: a successful grant ends the
            // episode, so retries against an already-broken balance do not
            // stack score penalties.
            if state.mark_breach()
                && let Some(reporter) = &self.reporter
            {
                reporter.report_peer(
                    &peer,
                    SwarmScoringEvent::AccountingViolation,
                    ReportSource::Accounting,
                );
            }
            return Err(AccountingError::DisconnectThreshold {
                peer,
                balance: current_balance,
                threshold: disconnect_threshold,
            });
        }
        state.clear_breach();

        state.add_reserved(price);
        Ok(ReceiveAction::new(state, price))
    }

    /// Prepare a provide action (we are providing service, balance increases).
    ///
    /// Hard serve-refuse (H3): refuse to serve once the peer's projected debt to
    /// us (committed balance plus outstanding provides plus this price) would
    /// cross the per-peer payment threshold, the point at which the peer is
    /// expected to settle. Without this gate a peer could free-ride up to the
    /// disconnect threshold per episode; the gate restores serve headroom only
    /// once the peer settles. The receive side keeps its own
    /// disconnect-threshold guard in [`Accounting::prepare_receive`].
    pub fn prepare_provide(
        &self,
        peer: OverlayAddress,
        price: Au,
    ) -> Result<ProvideAction, AccountingError> {
        let state = self.get_or_create_peer(peer);

        let payment_threshold = state.payment_threshold();
        // Projected debt the peer would owe us once this provide commits.
        let projected = state
            .balance()
            .saturating_add(state.shadow_reserved_balance())
            .saturating_add(price);
        if projected > payment_threshold {
            return Err(AccountingError::PaymentThreshold {
                peer,
                balance: projected,
                threshold: payment_threshold,
            });
        }

        state.add_shadow_reserved(price);
        Ok(ProvideAction::new(state, price))
    }

    /// Get or create peer state (double-checked locking).
    pub fn get_or_create_peer(&self, peer: OverlayAddress) -> Arc<PeerState> {
        // Fast path: read lock
        if let Some(state) = self.peers.read().get(&peer) {
            return Arc::clone(state);
        }

        // Slow path: write lock
        self.peers
            .write()
            .entry(peer)
            .or_insert_with(|| {
                Arc::new(PeerState::new(
                    self.config.payment_threshold(),
                    self.config.disconnect_threshold(),
                ))
            })
            .clone()
    }

    /// Get or create peer state for a Client peer, with thresholds scaled
    /// by the client-only factor.
    pub fn get_or_create_client_peer(&self, peer: OverlayAddress) -> Arc<PeerState> {
        if let Some(state) = self.peers.read().get(&peer) {
            return Arc::clone(state);
        }

        self.peers
            .write()
            .entry(peer)
            .or_insert_with(|| {
                Arc::new(PeerState::new_client_only(
                    self.config.payment_threshold(),
                    self.config.disconnect_threshold(),
                    self.config.client_only_factor(),
                ))
            })
            .clone()
    }
}

impl<C: SwarmAccountingConfig, I: SwarmIdentity> SwarmBandwidthAccounting for Accounting<C, I> {
    type Identity = I;
    type Peer = AccountingPeerHandle;
    type ReceiveAction = ReceiveAction;
    type ProvideAction = ProvideAction;

    fn identity(&self) -> &I {
        &self.identity
    }

    fn for_peer(&self, peer: OverlayAddress) -> Self::Peer {
        let state = self.get_or_create_peer(peer);
        AccountingPeerHandle {
            peer,
            state,
            providers: Arc::clone(&self.providers),
            disconnect_threshold: self.config.disconnect_threshold(),
            payment_threshold: self.config.payment_threshold(),
        }
    }

    fn peers(&self) -> Vec<OverlayAddress> {
        self.peers.read().keys().copied().collect()
    }

    fn remove_peer(&self, peer: &OverlayAddress) {
        self.peers.write().remove(peer);
    }

    fn prepare_receive(
        &self,
        peer: OverlayAddress,
        price: Au,
        originated: bool,
    ) -> SwarmResult<ReceiveAction> {
        Ok(Accounting::prepare_receive(self, peer, price, originated)?)
    }

    fn prepare_provide(&self, peer: OverlayAddress, price: Au) -> SwarmResult<ProvideAction> {
        Ok(Accounting::prepare_provide(self, peer, price)?)
    }
}

/// Affordability queries for the receive side of accounting.
///
/// Sign convention: `balance` is the peer's debt to us in AU (positive means
/// the peer owes us, negative means we owe the peer). Receiving service
/// debits our side, so a request of `price` moves the balance by `-price`.
/// A debit is affordable while the projected balance
/// (`balance - price - reserved`) stays at or above the negated disconnect
/// threshold, mirroring the guard in [`Accounting::prepare_receive`]. For
/// peers tracked with the standard thresholds, `can_afford(peer, price)` is
/// true exactly when a `prepare_receive` for `price` would succeed. The
/// per-peer threshold is read, so for client-only peers with scaled
/// thresholds affordability is stricter than the config-wide guard.
///
/// Unknown peers are treated as fresh zero-balance peers with the configured
/// default thresholds, matching [`Accounting::get_or_create_peer`]. The
/// queries are read-only and never insert peer state, so client-only
/// threshold scaling applies only once the peer record exists.
impl<C: SwarmAccountingConfig, I: SwarmIdentity> PeerAffordability for Accounting<C, I> {
    fn can_afford(&self, overlay: &OverlayAddress, price: Au) -> bool {
        price <= self.allowance_remaining(overlay)
    }

    fn allowance_remaining(&self, overlay: &OverlayAddress) -> Au {
        let (balance, reserved, threshold) = match self.peers.read().get(overlay) {
            Some(state) => (
                state.balance(),
                state.reserved_balance(),
                state.disconnect_threshold(),
            ),
            None => (Au::ZERO, Au::ZERO, self.config.disconnect_threshold()),
        };

        // Headroom is the signed balance plus the (non-negative) threshold less
        // the outstanding reservation, floored at zero. Saturating arithmetic
        // keeps the AU domain closed without an i128 detour.
        let headroom = balance.saturating_add(threshold).saturating_sub(reserved);
        headroom.max(Au::ZERO)
    }

    fn allowance_to_payment_threshold(&self, overlay: &OverlayAddress) -> Au {
        // Same headroom computation as `allowance_remaining`, but measured
        // against the payment threshold (the settlement trigger) instead of the
        // disconnect threshold. The payment threshold sits below the disconnect
        // threshold, so this is the headroom that stays strictly under the swap
        // trigger.
        let (balance, reserved, threshold) = match self.peers.read().get(overlay) {
            Some(state) => (
                state.balance(),
                state.reserved_balance(),
                state.payment_threshold(),
            ),
            None => (Au::ZERO, Au::ZERO, self.config.payment_threshold()),
        };

        let headroom = balance.saturating_add(threshold).saturating_sub(reserved);
        headroom.max(Au::ZERO)
    }
}

/// Handle to a peer's accounting state. Cheap to clone.
pub struct AccountingPeerHandle {
    peer: OverlayAddress,
    state: Arc<PeerState>,
    providers: Arc<[Box<dyn SwarmSettlementProvider>]>,
    disconnect_threshold: Au,
    payment_threshold: Au,
}

impl Clone for AccountingPeerHandle {
    fn clone(&self) -> Self {
        Self {
            peer: self.peer,
            state: Arc::clone(&self.state),
            providers: Arc::clone(&self.providers),
            disconnect_threshold: self.disconnect_threshold,
            payment_threshold: self.payment_threshold,
        }
    }
}

impl AccountingPeerHandle {
    /// Get access to the underlying peer state.
    pub fn state(&self) -> &Arc<PeerState> {
        &self.state
    }

    /// Get the payment threshold in AU.
    pub fn payment_threshold(&self) -> Au {
        self.payment_threshold
    }

    /// Get the disconnect threshold in AU.
    pub fn disconnect_threshold(&self) -> Au {
        self.disconnect_threshold
    }

    /// Call `pre_allow()` on all providers, returning total adjustment.
    fn pre_allow_all(&self) -> Au {
        self.providers
            .iter()
            .map(|p| p.pre_allow(self.peer, self.state.as_ref()))
            .sum()
    }

    /// Call `settle()` on providers in order until debt is below threshold.
    async fn settle_all(&self) -> SwarmResult<Au> {
        let mut total = Au::ZERO;

        for provider in self.providers.iter() {
            total = total.saturating_add(provider.settle(self.peer, self.state.as_ref()).await?);

            // Check if still over threshold
            let balance = self.state.balance();
            if balance <= self.payment_threshold {
                break;
            }
        }

        Ok(total)
    }
}

impl SwarmPeerBandwidth for AccountingPeerHandle {
    fn record(&self, amount: Au, direction: Direction) {
        match direction {
            Direction::Upload => self.state.add_balance(amount),
            // Saturating negation: `-amount` would wrap on `i64::MIN` (M8).
            Direction::Download => self.state.add_balance(Au::ZERO.saturating_sub(amount)),
        }
    }

    fn allow(&self, amount: Au) -> bool {
        // Let providers adjust balance first (e.g., pseudosettle refresh)
        self.pre_allow_all();

        // Check threshold
        let balance = self.state.balance();
        let reserved = self.state.reserved_balance();
        let projected = balance.saturating_sub(amount).saturating_sub(reserved);

        projected >= -self.disconnect_threshold
    }

    fn balance(&self) -> Au {
        self.state.balance()
    }

    async fn settle(&self) -> SwarmResult<()> {
        self.settle_all().await.map(|_| ())
    }

    fn peer(&self) -> OverlayAddress {
        self.peer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BandwidthConfig, NoSettlement};
    use vertex_swarm_test_utils::{Identity, test_identity, test_peer};

    fn test_accounting() -> Accounting<BandwidthConfig, Identity> {
        Accounting::new(BandwidthConfig::default(), test_identity())
    }

    fn au(value: i64) -> Au {
        Au::new(value)
    }

    #[test]
    fn test_accounting_basic() {
        let accounting = test_accounting();

        let handle = accounting.for_peer(test_peer());
        assert_eq!(handle.balance(), au(0));

        handle.record(au(1000), Direction::Upload);
        assert_eq!(handle.balance(), au(1000));

        handle.record(au(500), Direction::Download);
        assert_eq!(handle.balance(), au(500));
    }

    #[test]
    fn test_prepare_receive() {
        let accounting = test_accounting();

        let action = accounting
            .prepare_receive(test_peer(), au(1000), true)
            .expect("should prepare receive");

        let handle = accounting.for_peer(test_peer());
        assert_eq!(handle.state.reserved_balance(), au(1000));

        action.apply();

        assert_eq!(handle.balance(), au(-1000));
        assert_eq!(handle.state.reserved_balance(), au(0));
    }

    #[test]
    fn test_prepare_receive_dropped() {
        let accounting = test_accounting();

        {
            let _action = accounting
                .prepare_receive(test_peer(), au(1000), true)
                .expect("should prepare receive");
        }

        let handle = accounting.for_peer(test_peer());
        assert_eq!(handle.balance(), au(0));
        assert_eq!(handle.state.reserved_balance(), au(0));
    }

    #[test]
    fn test_with_single_provider() {
        let accounting = Accounting::with_providers(
            BandwidthConfig::default(),
            test_identity(),
            vec![Box::new(NoSettlement)],
        );

        let handle = accounting.for_peer(test_peer());
        assert_eq!(handle.balance(), au(0));

        handle.record(au(1000), Direction::Download);
        assert_eq!(handle.balance(), au(-1000));
    }

    #[test]
    fn test_with_two_providers() {
        let accounting = Accounting::with_providers(
            BandwidthConfig::default(),
            test_identity(),
            vec![Box::new(NoSettlement), Box::new(NoSettlement)],
        );

        let handle = accounting.for_peer(test_peer());
        assert_eq!(handle.balance(), au(0));

        handle.record(au(1000), Direction::Upload);
        assert_eq!(handle.balance(), au(1000));
    }

    #[test]
    fn test_allow_under_threshold() {
        let accounting = test_accounting();

        let handle = accounting.for_peer(test_peer());

        // Should allow small transfers
        assert!(handle.allow(au(1000)));

        // Record some debt
        handle.record(au(1000), Direction::Download);
        assert_eq!(handle.balance(), au(-1000));

        // Should still allow more (under disconnect threshold)
        assert!(handle.allow(au(1000)));
    }

    #[test]
    fn test_peers_list() {
        let accounting = test_accounting();

        let peer1 = OverlayAddress::from([1u8; 32]);
        let peer2 = OverlayAddress::from([2u8; 32]);

        let _ = accounting.for_peer(peer1);
        let _ = accounting.for_peer(peer2);

        let peers = accounting.peers();
        assert_eq!(peers.len(), 2);
        assert!(peers.contains(&peer1));
        assert!(peers.contains(&peer2));
    }

    #[test]
    fn test_remove_peer() {
        let accounting = test_accounting();

        let peer = test_peer();
        let _ = accounting.for_peer(peer);

        assert_eq!(accounting.peers().len(), 1);

        accounting.remove_peer(&peer);

        assert_eq!(accounting.peers().len(), 0);
    }

    #[test]
    fn test_handle_clone() {
        let accounting = test_accounting();

        let handle1 = accounting.for_peer(test_peer());
        let handle2 = handle1.clone();

        handle1.record(au(1000), Direction::Upload);

        // Both handles should see the same balance (shared state)
        assert_eq!(handle1.balance(), au(1000));
        assert_eq!(handle2.balance(), au(1000));
    }

    struct FixedAdjustProvider(Au);

    #[async_trait::async_trait]
    impl SwarmSettlementProvider for FixedAdjustProvider {
        fn pre_allow(
            &self,
            _peer: OverlayAddress,
            state: &dyn vertex_swarm_api::SwarmPeerState,
        ) -> Au {
            state.add_balance(self.0);
            self.0
        }

        async fn settle(
            &self,
            _peer: OverlayAddress,
            _state: &dyn vertex_swarm_api::SwarmPeerState,
        ) -> SwarmResult<Au> {
            Ok(Au::ZERO)
        }

        fn name(&self) -> &'static str {
            "fixed-adjust"
        }
    }

    #[test]
    fn test_provider_composition_pre_allow() {
        let accounting = Accounting::with_providers(
            BandwidthConfig::default(),
            test_identity(),
            vec![
                Box::new(FixedAdjustProvider(au(100))),
                Box::new(FixedAdjustProvider(au(200))),
            ],
        );

        let handle = accounting.for_peer(test_peer());

        // Trigger pre_allow via allow()
        handle.allow(au(0));

        // Both providers should have adjusted the balance
        assert_eq!(handle.balance(), au(300));
    }

    #[derive(Default)]
    struct RecordingReporter {
        reports: parking_lot::Mutex<Vec<(OverlayAddress, SwarmScoringEvent, ReportSource)>>,
    }

    impl PeerReporter for RecordingReporter {
        fn report_peer(
            &self,
            overlay: &OverlayAddress,
            event: SwarmScoringEvent,
            source: ReportSource,
        ) {
            self.reports.lock().push((*overlay, event, source));
        }
    }

    /// Config with payment threshold 1000 and 25% tolerance, so the
    /// disconnect threshold is 1250.
    fn small_config() -> BandwidthConfig {
        BandwidthConfig::new(
            1000,
            25,
            0,
            0,
            5,
            crate::constants::DEFAULT_THROTTLE_ALLOWANCE_PERCENT,
            crate::FixedPricingConfig::default(),
        )
    }

    const SMALL_DISCONNECT_THRESHOLD: Au = Au::new(1250);

    #[test]
    fn test_violation_reported_once_per_breach_episode() {
        let reporter = Arc::new(RecordingReporter::default());
        let accounting = Accounting::new(small_config(), test_identity())
            .with_reporter(Arc::clone(&reporter) as Arc<dyn PeerReporter>);
        let peer = test_peer();

        // First breach reports exactly once.
        assert!(matches!(
            accounting.prepare_receive(peer, au(2000), true),
            Err(AccountingError::DisconnectThreshold { .. })
        ));
        assert_eq!(reporter.reports.lock().len(), 1);
        let (reported_peer, event, source) = reporter.reports.lock()[0];
        assert_eq!(reported_peer, peer);
        assert_eq!(event, SwarmScoringEvent::AccountingViolation);
        assert_eq!(source, ReportSource::Accounting);

        // Retrying against the same broken state does not stack reports.
        assert!(accounting.prepare_receive(peer, au(2000), true).is_err());
        assert!(accounting.prepare_receive(peer, au(3000), true).is_err());
        assert_eq!(reporter.reports.lock().len(), 1);

        // A successful grant ends the episode...
        let action = accounting
            .prepare_receive(peer, au(100), true)
            .expect("within threshold");
        drop(action);
        assert_eq!(reporter.reports.lock().len(), 1);

        // ...so the next breach is a new episode and reports again.
        assert!(accounting.prepare_receive(peer, au(2000), true).is_err());
        assert_eq!(reporter.reports.lock().len(), 2);
    }

    #[test]
    fn test_no_reporter_behaviour_unchanged() {
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        assert!(matches!(
            accounting.prepare_receive(peer, au(2000), true),
            Err(AccountingError::DisconnectThreshold { .. })
        ));

        let action = accounting
            .prepare_receive(peer, SMALL_DISCONNECT_THRESHOLD, true)
            .expect("exactly at threshold is allowed");
        action.apply();

        let handle = accounting.for_peer(peer);
        assert_eq!(handle.balance(), -SMALL_DISCONNECT_THRESHOLD);
    }

    #[test]
    fn test_affordability_unknown_peer_is_fresh_and_read_only() {
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        // Unknown peers are treated as fresh zero-balance peers.
        assert_eq!(
            accounting.allowance_remaining(&peer),
            SMALL_DISCONNECT_THRESHOLD
        );
        assert!(accounting.can_afford(&peer, SMALL_DISCONNECT_THRESHOLD));
        assert!(!accounting.can_afford(&peer, SMALL_DISCONNECT_THRESHOLD + Au::new(1)));

        // Affordability queries never insert peer state.
        assert!(accounting.peers().is_empty());
    }

    #[test]
    fn test_affordability_boundaries_match_prepare_receive() {
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        // Build up debt: we owe the peer 500 AU.
        let handle = accounting.for_peer(peer);
        handle.record(au(500), Direction::Download);
        assert_eq!(handle.balance(), au(-500));
        assert_eq!(accounting.allowance_remaining(&peer), au(750));

        // Exactly at the threshold: affordable and grantable.
        assert!(accounting.can_afford(&peer, au(750)));
        assert!(accounting.prepare_receive(peer, au(750), true).is_ok());

        // Just over: refused by both.
        assert!(!accounting.can_afford(&peer, au(751)));
        assert!(accounting.prepare_receive(peer, au(751), true).is_err());
    }

    #[test]
    fn test_affordability_accounts_for_reservations() {
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        let action = accounting
            .prepare_receive(peer, au(1000), true)
            .expect("within threshold");

        // The outstanding reservation consumes headroom.
        assert_eq!(accounting.allowance_remaining(&peer), au(250));
        assert!(accounting.can_afford(&peer, au(250)));
        assert!(!accounting.can_afford(&peer, au(251)));

        // Releasing the reservation restores the headroom.
        drop(action);
        assert_eq!(
            accounting.allowance_remaining(&peer),
            SMALL_DISCONNECT_THRESHOLD
        );
    }

    #[test]
    fn test_provide_refused_past_payment_threshold_until_settled() {
        // Payment threshold 1000, disconnect 1250.
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();
        let handle = accounting.for_peer(peer);

        // Serving up to the payment threshold is allowed.
        let provide = accounting
            .prepare_provide(peer, au(1000))
            .expect("at payment threshold is allowed");
        provide.apply();
        assert_eq!(handle.balance(), au(1000));

        // The peer now owes us exactly the payment threshold. Any further
        // service is refused: it would push the projected debt over the
        // threshold (H3 free-ride stop), even though the disconnect threshold
        // (1250) has not been reached.
        assert!(matches!(
            accounting.prepare_provide(peer, au(1)),
            Err(AccountingError::PaymentThreshold { .. })
        ));

        // The peer settles (we forgive/receive its debt), restoring headroom.
        handle.record(au(600), Direction::Download);
        assert_eq!(handle.balance(), au(400));

        // Service resumes within the recovered headroom.
        let provide = accounting
            .prepare_provide(peer, au(600))
            .expect("settled peer is served again");
        provide.apply();
        assert_eq!(handle.balance(), au(1000));
    }

    #[test]
    fn test_provide_refusal_counts_outstanding_reservations() {
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        // An outstanding (un-applied) provide reserves shadow balance, so a
        // second provide that together crosses the threshold is refused.
        let _outstanding = accounting
            .prepare_provide(peer, au(900))
            .expect("first provide reserved");
        assert!(matches!(
            accounting.prepare_provide(peer, au(200)),
            Err(AccountingError::PaymentThreshold { .. })
        ));
        // A smaller provide that stays under the threshold still succeeds.
        assert!(accounting.prepare_provide(peer, au(100)).is_ok());
    }

    #[test]
    fn test_payment_threshold_headroom_is_below_disconnect_headroom() {
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        // Payment threshold is 1000 (settlement trigger); disconnect threshold is
        // 1250. The payment-threshold headroom is narrower and sits strictly below
        // the disconnect-threshold headroom for both unknown and known peers.
        assert_eq!(accounting.allowance_to_payment_threshold(&peer), au(1000));
        assert_eq!(accounting.allowance_remaining(&peer), au(1250));

        // Debt narrows both headrooms by the same amount, keeping the payment
        // figure below the disconnect figure.
        let handle = accounting.for_peer(peer);
        handle.record(au(400), Direction::Download);
        assert_eq!(handle.balance(), au(-400));
        assert_eq!(accounting.allowance_to_payment_threshold(&peer), au(600));
        assert_eq!(accounting.allowance_remaining(&peer), au(850));
    }
}
