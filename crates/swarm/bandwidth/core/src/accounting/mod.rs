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
    Direction, SwarmAccountingConfig, SwarmBandwidthAccounting, SwarmIdentity, SwarmPeerBandwidth,
    SwarmResult,
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
    /// Providers are called in order during settlement operations.
    /// For `BandwidthMode::Both`, pseudosettle should come before swap.
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
        price: u64,
        _originated: bool,
    ) -> Result<ReceiveAction, AccountingError> {
        let state = self.get_or_create_peer(peer);

        let current_balance = state.balance();
        let reserved = state.reserved_balance();
        let projected = current_balance - (price as i64) - (reserved as i64);

        let disconnect_threshold = self.config.disconnect_threshold();
        let threshold = -(disconnect_threshold as i64);
        if projected < threshold {
            return Err(AccountingError::DisconnectThreshold {
                peer,
                balance: current_balance,
                threshold: disconnect_threshold,
            });
        }

        state.add_reserved(price);
        Ok(ReceiveAction::new(state, price))
    }

    /// Prepare a provide action (we are providing service, balance increases).
    pub fn prepare_provide(
        &self,
        peer: OverlayAddress,
        price: u64,
    ) -> Result<ProvideAction, AccountingError> {
        let state = self.get_or_create_peer(peer);
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

    /// Get or create peer state for a light node.
    pub fn get_or_create_light_peer(&self, peer: OverlayAddress) -> Arc<PeerState> {
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
        price: u64,
        originated: bool,
    ) -> SwarmResult<ReceiveAction> {
        Accounting::prepare_receive(self, peer, price, originated)
            .map_err(|e| vertex_swarm_api::SwarmError::Accounting(e.to_string()))
    }

    fn prepare_provide(&self, peer: OverlayAddress, price: u64) -> SwarmResult<ProvideAction> {
        Accounting::prepare_provide(self, peer, price)
            .map_err(|e| vertex_swarm_api::SwarmError::Accounting(e.to_string()))
    }
}

/// Handle to a peer's accounting state. Cheap to clone.
pub struct AccountingPeerHandle {
    peer: OverlayAddress,
    state: Arc<PeerState>,
    providers: Arc<[Box<dyn SwarmSettlementProvider>]>,
    disconnect_threshold: u64,
    payment_threshold: u64,
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

    /// Get the payment threshold.
    pub fn payment_threshold(&self) -> u64 {
        self.payment_threshold
    }

    /// Get the disconnect threshold.
    pub fn disconnect_threshold(&self) -> u64 {
        self.disconnect_threshold
    }

    /// Call `pre_allow()` on all providers, returning total adjustment.
    fn pre_allow_all(&self) -> i64 {
        self.providers
            .iter()
            .map(|p| p.pre_allow(self.peer, self.state.as_ref()))
            .sum()
    }

    /// Call `settle()` on providers in order until debt is below threshold.
    async fn settle_all(&self) -> SwarmResult<i64> {
        let mut total = 0i64;

        for provider in self.providers.iter() {
            total = total.saturating_add(provider.settle(self.peer, self.state.as_ref()).await?);

            // Check if still over threshold
            let balance = self.state.balance();
            if balance <= self.payment_threshold as i64 {
                break;
            }
        }

        Ok(total)
    }
}

#[async_trait::async_trait]
impl SwarmPeerBandwidth for AccountingPeerHandle {
    fn record(&self, bytes: u64, direction: Direction) {
        match direction {
            Direction::Upload => self.state.add_balance(bytes as i64),
            Direction::Download => self.state.add_balance(-(bytes as i64)),
        }
    }

    fn allow(&self, bytes: u64) -> bool {
        // Let providers adjust balance first (e.g., pseudosettle refresh)
        self.pre_allow_all();

        // Check threshold
        let balance = self.state.balance();
        let reserved = self.state.reserved_balance();
        let projected = balance - (bytes as i64) - (reserved as i64);

        projected >= -(self.disconnect_threshold as i64)
    }

    fn balance(&self) -> i64 {
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
    use crate::{DefaultAccountingConfig, NoSettlement};
    use vertex_swarm_api::SwarmNodeType;
    use vertex_swarm_identity::Identity;

    fn test_identity() -> Identity {
        Identity::random(vertex_swarmspec::init_testnet(), SwarmNodeType::Client)
    }

    fn test_peer() -> OverlayAddress {
        OverlayAddress::from([1u8; 32])
    }

    fn test_accounting() -> Accounting<DefaultAccountingConfig, Identity> {
        Accounting::new(DefaultAccountingConfig, test_identity())
    }

    #[test]
    fn test_accounting_basic() {
        let accounting = test_accounting();

        let handle = accounting.for_peer(test_peer());
        assert_eq!(handle.balance(), 0);

        handle.record(1000, Direction::Upload);
        assert_eq!(handle.balance(), 1000);

        handle.record(500, Direction::Download);
        assert_eq!(handle.balance(), 500);
    }

    #[test]
    fn test_prepare_receive() {
        let accounting = test_accounting();

        let action = accounting
            .prepare_receive(test_peer(), 1000, true)
            .expect("should prepare receive");

        let handle = accounting.for_peer(test_peer());
        assert_eq!(handle.state.reserved_balance(), 1000);

        action.apply();

        assert_eq!(handle.balance(), -1000);
        assert_eq!(handle.state.reserved_balance(), 0);
    }

    #[test]
    fn test_prepare_receive_dropped() {
        let accounting = test_accounting();

        {
            let _action = accounting
                .prepare_receive(test_peer(), 1000, true)
                .expect("should prepare receive");
        }

        let handle = accounting.for_peer(test_peer());
        assert_eq!(handle.balance(), 0);
        assert_eq!(handle.state.reserved_balance(), 0);
    }

    #[test]
    fn test_with_single_provider() {
        let accounting = Accounting::with_providers(
            DefaultAccountingConfig,
            test_identity(),
            vec![Box::new(NoSettlement)],
        );

        let handle = accounting.for_peer(test_peer());
        assert_eq!(handle.balance(), 0);

        handle.record(1000, Direction::Download);
        assert_eq!(handle.balance(), -1000);
    }

    #[test]
    fn test_with_two_providers() {
        let accounting = Accounting::with_providers(
            DefaultAccountingConfig,
            test_identity(),
            vec![Box::new(NoSettlement), Box::new(NoSettlement)],
        );

        let handle = accounting.for_peer(test_peer());
        assert_eq!(handle.balance(), 0);

        handle.record(1000, Direction::Upload);
        assert_eq!(handle.balance(), 1000);
    }

    #[test]
    fn test_allow_under_threshold() {
        let accounting = test_accounting();

        let handle = accounting.for_peer(test_peer());

        // Should allow small transfers
        assert!(handle.allow(1000));

        // Record some debt
        handle.record(1000, Direction::Download);
        assert_eq!(handle.balance(), -1000);

        // Should still allow more (under disconnect threshold)
        assert!(handle.allow(1000));
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

        handle1.record(1000, Direction::Upload);

        // Both handles should see the same balance (shared state)
        assert_eq!(handle1.balance(), 1000);
        assert_eq!(handle2.balance(), 1000);
    }

    struct FixedAdjustProvider(i64);

    #[async_trait::async_trait]
    impl SwarmSettlementProvider for FixedAdjustProvider {
        fn supported_mode(&self) -> vertex_swarm_api::BandwidthMode {
            vertex_swarm_api::BandwidthMode::None
        }

        fn pre_allow(
            &self,
            _peer: OverlayAddress,
            state: &dyn vertex_swarm_api::SwarmPeerState,
        ) -> i64 {
            state.add_balance(self.0);
            self.0
        }

        async fn settle(
            &self,
            _peer: OverlayAddress,
            _state: &dyn vertex_swarm_api::SwarmPeerState,
        ) -> SwarmResult<i64> {
            Ok(0)
        }

        fn name(&self) -> &'static str {
            "fixed-adjust"
        }
    }

    #[test]
    fn test_provider_composition_pre_allow() {
        let accounting = Accounting::with_providers(
            DefaultAccountingConfig,
            test_identity(),
            vec![
                Box::new(FixedAdjustProvider(100)),
                Box::new(FixedAdjustProvider(200)),
            ],
        );

        let handle = accounting.for_peer(test_peer());

        // Trigger pre_allow via allow()
        handle.allow(0);

        // Both providers should have adjusted the balance
        assert_eq!(handle.balance(), 300);
    }
}
