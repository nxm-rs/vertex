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
//! - [`Reservation`] - Typed receive/provide reservation legs

mod error;
mod peer;
mod reservation;

pub use error::AccountingError;
pub use peer::PeerState;
pub use reservation::{Provide, Receive, Reservation};

use alloc::vec::Vec;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

use vertex_swarm_api::{
    AdmissionControl, Au, Debt, Direction, Ledger, SwarmAccountingConfig, SwarmBandwidthAccounting,
    SwarmIdentity, SwarmPeerBandwidth, SwarmResult, Threshold,
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
}

impl<C: SwarmAccountingConfig, I: SwarmIdentity> Accounting<C, I> {
    /// Create a new accounting instance with no settlement providers.
    pub fn new(config: C, identity: I) -> Self {
        Self {
            config,
            identity,
            providers: Arc::from(Vec::new()),
            peers: RwLock::new(HashMap::new()),
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
        }
    }

    /// Returns the names of the active settlement providers.
    pub fn provider_names(&self) -> Vec<&str> {
        self.providers.iter().map(|p| p.name()).collect()
    }

    /// Prepare a receive reservation (we are receiving service, balance decreases).
    ///
    /// The hard gate shares one boundary with the advisory [`AdmissionControl::admit`]:
    /// it calls `admit` and refuses a [`Refuse`](vertex_swarm_api::Admission::Refuse)
    /// band. The breach is never scored against the peer; our debt reaching our own
    /// disconnect line is a local pacing outcome, not peer misbehaviour, and the
    /// remote enforces its own view by refusing or resetting us.
    pub fn prepare_receive(
        &self,
        peer: OverlayAddress,
        price: Au,
        _originated: bool,
    ) -> Result<Reservation<Receive>, AccountingError> {
        if !AdmissionControl::admit(self, &peer, price).admits() {
            return Err(AccountingError::DisconnectThreshold {
                peer,
                balance: Ledger::balance(self, &peer),
                threshold: self.config.disconnect_threshold(),
            });
        }

        let state = self.get_or_create_peer(peer);
        state.add_reserved(price);
        Ok(Reservation::new(state, price))
    }

    /// Prepare a provide action (we are providing service, balance increases).
    ///
    /// Refuse to serve once the peer's projected debt to us (committed balance
    /// plus outstanding provides plus this price) would cross the per-peer
    /// payment threshold, the point at which the peer is expected to settle.
    /// Without this gate a peer could free-ride up to the disconnect threshold
    /// per episode; the gate restores serve headroom only once the peer settles.
    /// The receive side keeps its own disconnect-threshold guard in
    /// [`Accounting::prepare_receive`].
    pub fn prepare_provide(
        &self,
        peer: OverlayAddress,
        price: Au,
    ) -> Result<Reservation<Provide>, AccountingError> {
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
        Ok(Reservation::new(state, price))
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
}

impl<C: SwarmAccountingConfig, I: SwarmIdentity> SwarmBandwidthAccounting for Accounting<C, I> {
    type Identity = I;
    type Peer = AccountingPeerHandle;
    type ReceiveAction = Reservation<Receive>;
    type ProvideAction = Reservation<Provide>;

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
    ) -> SwarmResult<Reservation<Receive>> {
        Ok(Accounting::prepare_receive(self, peer, price, originated)?)
    }

    fn prepare_provide(
        &self,
        peer: OverlayAddress,
        price: Au,
    ) -> SwarmResult<Reservation<Provide>> {
        Ok(Accounting::prepare_provide(self, peer, price)?)
    }
}

/// Per-peer ledger reads for admission and self-throttling.
///
/// Sign convention: `balance` is the peer's debt to us in AU (positive means the
/// peer owes us, negative we owe the peer). `headroom` is the floored allowance
/// toward a threshold; the admission band that consumes it lives in the default
/// [`AdmissionControl::admit`]. Unknown peers read as fresh zero-balance peers
/// with the configured thresholds, matching [`Accounting::get_or_create_peer`],
/// and the reads never insert peer state.
impl<C: SwarmAccountingConfig, I: SwarmIdentity> Ledger for Accounting<C, I> {
    fn balance(&self, peer: &OverlayAddress) -> Au {
        self.peers
            .read()
            .get(peer)
            .map_or(Au::ZERO, |state| state.balance())
    }

    fn reserved(&self, peer: &OverlayAddress) -> Au {
        self.peers
            .read()
            .get(peer)
            .map_or(Au::ZERO, |state| state.reserved_balance())
    }

    fn headroom(&self, peer: &OverlayAddress, to: Threshold) -> Au {
        let (balance, reserved, threshold) = match self.peers.read().get(peer) {
            Some(state) => (
                state.balance(),
                state.reserved_balance(),
                match to {
                    Threshold::Payment => state.payment_threshold(),
                    Threshold::Disconnect => state.disconnect_threshold(),
                },
            ),
            None => (
                Au::ZERO,
                Au::ZERO,
                match to {
                    Threshold::Payment => self.config.payment_threshold(),
                    Threshold::Disconnect => self.config.disconnect_threshold(),
                },
            ),
        };

        // The signed balance plus the (non-negative) threshold less the
        // outstanding reservation, floored at zero. Saturating arithmetic keeps
        // the AU domain closed without an i128 detour.
        balance
            .saturating_add(threshold)
            .saturating_sub(reserved)
            .max(Au::ZERO)
    }

    fn disconnect_line(&self, peer: &OverlayAddress) -> Au {
        self.peers.read().get(peer).map_or_else(
            || self.config.disconnect_threshold(),
            |state| state.disconnect_threshold(),
        )
    }

    fn settle_trigger(&self, _peer: &OverlayAddress) -> Au {
        // The early-payment trigger floored at one refresh-rate unit, so a settle
        // always offers at least the minimum the peer acts on. Per-peer state
        // carries no early-payment figure, so this reads from config.
        self.config
            .early_payment_trigger()
            .max(self.config.refresh_rate())
    }
}

/// Handle to a peer's accounting state. Cheap to clone.
#[derive(Clone)]
pub struct AccountingPeerHandle {
    peer: OverlayAddress,
    state: Arc<PeerState>,
    providers: Arc<[Box<dyn SwarmSettlementProvider>]>,
    disconnect_threshold: Au,
    payment_threshold: Au,
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

    /// Call `settle()` on providers in order until debt is below threshold.
    async fn settle_all(&self) -> SwarmResult<Au> {
        let mut total = Au::ZERO;

        for provider in self.providers.iter() {
            total = total.saturating_add(provider.settle(self.peer, self.state.as_ref()).await?);

            // Stop once the committed debt no longer exceeds the payment
            // threshold. Reasoning in `Debt` keeps the comparison sign-safe (both
            // sides non-negative); each provider re-reads `balance()` internally,
            // so the fresh committed debt drives the break.
            if !Debt::committed(self.state.balance()).exceeds(self.payment_threshold) {
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
            Direction::Download => self.state.add_balance(-amount),
        }
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
    use vertex_swarm_api::Admission;
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

    /// The default storer config scaled to the line a storer enforces on a
    /// client: payment 1_350_000, disconnect 1_687_500, settle trigger 675_000.
    fn client_config() -> BandwidthConfig {
        BandwidthConfig::default().for_client()
    }

    #[test]
    fn test_receive_breach_refuses_without_scoring_the_peer() {
        // A debtor breaching its own disconnect line refuses the receive so the
        // caller routes elsewhere, but never scores the creditor: the debt
        // reaching our own line is a local pacing outcome, not peer
        // misbehaviour. Accounting holds no reporter, so a breach can never feed
        // peer scoring.
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        assert!(matches!(
            accounting.prepare_receive(peer, au(2000), true),
            Err(AccountingError::DisconnectThreshold { .. })
        ));

        // Retrying against the broken state keeps refusing.
        assert!(accounting.prepare_receive(peer, au(2000), true).is_err());
        assert!(accounting.prepare_receive(peer, au(3000), true).is_err());

        // A receive within the line is granted again, then a fresh breach
        // refuses once more.
        let action = accounting
            .prepare_receive(peer, au(100), true)
            .expect("within threshold");
        drop(action);
        assert!(accounting.prepare_receive(peer, au(2000), true).is_err());
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
    fn test_admit_unknown_peer_is_fresh_and_read_only() {
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        // Unknown peers are treated as fresh zero-balance peers, so the
        // disconnect headroom is the full threshold.
        assert_eq!(
            accounting.headroom(&peer, Threshold::Disconnect),
            SMALL_DISCONNECT_THRESHOLD
        );
        assert!(accounting.admit(&peer, SMALL_DISCONNECT_THRESHOLD).admits());
        assert!(
            !accounting
                .admit(&peer, SMALL_DISCONNECT_THRESHOLD + Au::new(1))
                .admits()
        );

        // Ledger reads never insert peer state.
        assert!(accounting.peers().is_empty());
    }

    #[test]
    fn test_admit_boundary_is_the_prepare_receive_boundary() {
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        // Build up debt: we owe the peer 500 AU.
        let handle = accounting.for_peer(peer);
        handle.record(au(500), Direction::Download);
        assert_eq!(handle.balance(), au(-500));
        assert_eq!(accounting.headroom(&peer, Threshold::Disconnect), au(750));

        // Exactly at the threshold: admitted and grantable (prepare_receive
        // routes through the same admit boundary).
        assert!(accounting.admit(&peer, au(750)).admits());
        assert!(accounting.prepare_receive(peer, au(750), true).is_ok());

        // Just over: refused by both, because they are one boundary.
        assert!(!accounting.admit(&peer, au(751)).admits());
        assert!(accounting.prepare_receive(peer, au(751), true).is_err());
    }

    #[test]
    fn test_admit_accounts_for_reservations() {
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        let action = accounting
            .prepare_receive(peer, au(1000), true)
            .expect("within threshold");

        // The outstanding reservation consumes headroom.
        assert_eq!(accounting.headroom(&peer, Threshold::Disconnect), au(250));
        assert!(accounting.admit(&peer, au(250)).admits());
        assert!(!accounting.admit(&peer, au(251)).admits());

        // Releasing the reservation restores the headroom.
        drop(action);
        assert_eq!(
            accounting.headroom(&peer, Threshold::Disconnect),
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
        // threshold, even though the disconnect threshold (1250) has not been
        // reached.
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
        assert_eq!(accounting.headroom(&peer, Threshold::Payment), au(1000));
        assert_eq!(accounting.headroom(&peer, Threshold::Disconnect), au(1250));

        // Debt narrows both headrooms by the same amount, keeping the payment
        // figure below the disconnect figure.
        let handle = accounting.for_peer(peer);
        handle.record(au(400), Direction::Download);
        assert_eq!(handle.balance(), au(-400));
        assert_eq!(accounting.headroom(&peer, Threshold::Payment), au(600));
        assert_eq!(accounting.headroom(&peer, Threshold::Disconnect), au(850));
    }

    #[test]
    fn test_admit_bands_settle_between_payment_and_disconnect() {
        // Payment 1000, disconnect 1250. A request landing the projected debt in
        // (payment, disconnect] is SettleAndAdmit; below is Admit; above Refuse.
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        assert_eq!(accounting.admit(&peer, au(1000)), Admission::Admit);
        assert_eq!(accounting.admit(&peer, au(1001)), Admission::SettleAndAdmit);
        assert_eq!(accounting.admit(&peer, au(1250)), Admission::SettleAndAdmit);
        assert_eq!(accounting.admit(&peer, au(1251)), Admission::Refuse);
    }

    /// Reports settling a fixed amount, modelling a provider that pays only part
    /// of a large debt. In production the tracked balance is reduced by the
    /// async service ack, not by the provider, so the mock leaves the state arg
    /// untouched.
    struct PartialSettleProvider(Au);

    #[async_trait::async_trait]
    impl SwarmSettlementProvider for PartialSettleProvider {
        async fn settle(
            &self,
            _peer: OverlayAddress,
            _state: &dyn vertex_swarm_api::SwarmPeerState,
        ) -> SwarmResult<Au> {
            Ok(self.0)
        }

        fn name(&self) -> &'static str {
            "partial-settle"
        }
    }

    /// Records whether its `settle` ran.
    struct RecordingProvider(Arc<std::sync::atomic::AtomicBool>);

    #[async_trait::async_trait]
    impl SwarmSettlementProvider for RecordingProvider {
        async fn settle(
            &self,
            _peer: OverlayAddress,
            _state: &dyn vertex_swarm_api::SwarmPeerState,
        ) -> SwarmResult<Au> {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(Au::ZERO)
        }

        fn name(&self) -> &'static str {
            "recording"
        }
    }

    #[tokio::test]
    async fn settle_all_reaches_every_provider_while_debt_remains() {
        // Payment threshold 1000. A 5000 debt stays past the threshold while the
        // first provider settles only part of it, so the fan-out must run the
        // second provider too.
        let ran_second = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let accounting = Accounting::with_providers(
            small_config(),
            test_identity(),
            vec![
                Box::new(PartialSettleProvider(au(1000))),
                Box::new(RecordingProvider(Arc::clone(&ran_second))),
            ],
        );

        let handle = accounting.for_peer(test_peer());
        handle.record(au(5000), Direction::Download);
        assert_eq!(handle.balance(), au(-5000));

        handle.settle().await.expect("settle succeeds");

        assert!(
            ran_second.load(std::sync::atomic::Ordering::SeqCst),
            "the second provider must run while debt remains past the threshold"
        );
    }

    #[test]
    fn admit_settles_once_the_request_crosses_the_payment_threshold() {
        // Payment 1000, disconnect 1250. A fresh request that lands the projected
        // debt past the payment threshold settles; one that stays below does not.
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        // We already owe 900; a 50 AU request stays under the 1000 payment line.
        let handle = accounting.for_peer(peer);
        handle.record(au(900), Direction::Download);
        assert!(!accounting.admit(&peer, au(50)).settles());

        // A 200 AU request lands the projected debt at 1100, past the payment
        // line but below disconnect: settle.
        assert!(accounting.admit(&peer, au(200)).settles());
    }

    #[test]
    fn admit_at_zero_price_settles_once_committed_debt_passes_the_settle_trigger() {
        // The client settle path calls `admit(peer, Au::ZERO).settles()`. With our
        // debt already past the payment threshold (1_350_000) but below the
        // disconnect line, a zero-price band must still settle. The earlier
        // floored-headroom reconstruction collapsed to `price > 0` here and stopped
        // settling exactly when the debt most needed paying down.
        let accounting = Accounting::new(client_config(), test_identity());
        let peer = test_peer();

        let handle = accounting.for_peer(peer);
        handle.record(au(1_500_000), Direction::Download);
        assert_eq!(handle.balance(), au(-1_500_000));

        assert!(
            accounting.admit(&peer, Au::ZERO).settles(),
            "a client over its payment threshold must still settle at zero price"
        );
    }

    #[test]
    fn admit_refuse_boundary_is_the_prepare_receive_boundary_at_the_disconnect_line() {
        // Payment 1000, disconnect 1250. The refuse band is the original
        // prepare_receive boundary: refuse exactly when the projected debt crosses
        // the disconnect line, and `prepare_receive` errors at the same point.
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        // Fresh peer: the projected debt equals the price. At the disconnect line
        // the request is still admitted; one unit past it is refused.
        assert_ne!(accounting.admit(&peer, au(1250)), Admission::Refuse);
        assert_eq!(accounting.admit(&peer, au(1251)), Admission::Refuse);

        assert!(accounting.prepare_receive(peer, au(1250), true).is_ok());
        assert!(matches!(
            accounting.prepare_receive(peer, au(1251), true),
            Err(AccountingError::DisconnectThreshold { .. })
        ));
    }

    #[test]
    fn admit_settles_at_the_early_payment_trigger_not_the_payment_threshold() {
        // Payment 1000, disconnect 1250, early-payment 40% so the settle trigger is
        // 600, strictly below the payment threshold. A projected debt below 600
        // admits; at or above (and below disconnect) settles. This pins the settle
        // point to the early-payment value, not the full payment threshold.
        let config = BandwidthConfig::new(
            1000,
            25,
            10,
            40,
            5,
            crate::constants::DEFAULT_THROTTLE_ALLOWANCE_PERCENT,
            crate::FixedPricingConfig::default(),
        );
        let accounting = Accounting::new(config, test_identity());
        let peer = test_peer();

        assert_eq!(accounting.admit(&peer, au(600)), Admission::Admit);
        assert_eq!(accounting.admit(&peer, au(601)), Admission::SettleAndAdmit);
        assert_eq!(accounting.admit(&peer, au(1250)), Admission::SettleAndAdmit);
        assert_eq!(accounting.admit(&peer, au(1251)), Admission::Refuse);
    }

    #[test]
    fn held_receive_reservation_raises_projected_but_not_committed_debt() {
        // A held un-applied receive reservation raises the admission `project`
        // debt (it consumes headroom) but leaves the committed debt that drives
        // settlement unchanged, so a cheque never pays for a reservation that can
        // still drop.
        let accounting = Accounting::new(small_config(), test_identity());
        let peer = test_peer();

        let reservation = accounting
            .prepare_receive(peer, au(800), true)
            .expect("within threshold");

        let balance = Ledger::balance(&accounting, &peer);
        let reserved = Ledger::reserved(&accounting, &peer);
        // Committed debt ignores the reservation (balance is still zero).
        assert_eq!(Debt::committed(balance), Debt::ZERO);
        // Projected debt for a further request includes the held reservation
        // (800 reserved + 100 price = 900), even though committed debt is zero.
        assert_eq!(Au::from(Debt::project(balance, reserved, au(100))), au(900));

        drop(reservation);
        assert_eq!(Ledger::reserved(&accounting, &peer), Au::ZERO);
    }
}
