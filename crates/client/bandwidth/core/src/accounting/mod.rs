//! Per-peer accounting for bandwidth incentives.
//!
//! This module provides the core accounting infrastructure:
//!
//! - **PeerState**: Atomic per-peer balance tracking
//! - **Accounting**: Factory implementing `BandwidthAccounting` trait with pluggable settlement
//! - **Actions**: `CreditAction` and `DebitAction` for prepare/apply pattern
//!
//! Settlement logic (pseudosettle, swap) is in separate crates and plugged in
//! via [`SettlementProvider`](crate::settlement::SettlementProvider).
//!
//! # What is an Accounting Unit (AU)?
//!
//! An **Accounting Unit (AU)** is an abstract unit of measure for bandwidth
//! accounting in Swarm. It exists to solve a specific problem: how do you
//! fairly price chunk retrieval when the cost varies based on network distance?
//!
//! ## The Problem
//!
//! When you request a chunk from the Swarm network:
//! - If the chunk is "close" to you in Kademlia space, it takes fewer network
//!   hops to retrieve (cheaper for the network to serve)
//! - If the chunk is "far" from you, it takes more hops (more expensive)
//!
//! Simply counting bytes doesn't capture this cost difference.
//!
//! ## The Solution: Accounting Units
//!
//! AUs are protocol-defined values that encode both:
//! 1. The fact that a chunk was transferred
//! 2. The network cost based on proximity (distance in Kademlia space)
//!
//! ## Key Properties of AUs
//!
//! - **NOT bytes**: One chunk is always 4KB of data, but costs variable AUs
//! - **NOT BZZ tokens**: AUs are for accounting, BZZ is for actual payment
//! - **NOT directly convertible**: The AU↔BZZ exchange rate is protocol-defined
//!
//! ## The Pricing Formula
//!
//! ```text
//! chunk_price_au = (max_po - proximity + 1) × base_price
//! ```
//!
//! Where:
//! - `max_po` = maximum proximity order from `SwarmSpec` (31 for standard networks)
//! - `proximity` = number of leading bits two addresses share (0-31)
//! - `base_price = 10,000 AU` (default)
//!
//! ## Examples
//!
//! | Proximity | Multiplier | Price (AU) | Interpretation |
//! |-----------|------------|------------|----------------|
//! | 31 (same) | 1          | 10,000     | Chunk is in your neighborhood |
//! | 16 (mid)  | 16         | 160,000    | Chunk is moderately far |
//! | 0 (far)   | 32         | 320,000    | Chunk is maximally distant |
//!
//! ## Relationship to Settlement
//!
//! AUs accumulate as debt between peers. When debt reaches a threshold:
//! - **Pseudosettle**: Debt is forgiven over time (time-based allowance)
//! - **SWAP**: Debt is settled with actual BZZ token payments
//!
//! # Architecture
//!
//! ```text
//! Accounting<C, I>
//! ├── config: C (AccountingConfig)
//! ├── identity: I (Identity)
//! ├── providers: Arc<[Box<dyn SettlementProvider>]>
//! └── peers: RwLock<HashMap<OverlayAddress, Arc<PeerState>>>
//!           │
//!           └── AccountingPeerHandle
//!               ├── state: Arc<PeerState>
//!               └── providers: Arc<[Box<dyn SettlementProvider>]>
//! ```

mod action;
mod error;
mod peer;

pub use action::{AccountingAction, CreditAction, DebitAction};
pub use error::AccountingError;
pub use peer::PeerState;

use alloc::vec::Vec;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

use vertex_primitives::OverlayAddress;
use vertex_swarm_api::{
    SwarmAccountingConfig, SwarmBandwidthAccounting, Direction, SwarmIdentity, SwarmPeerBandwidth, SwarmError,
    SwarmResult,
};

use crate::settlement::SettlementProvider;

/// Core accounting implementation with pluggable settlement providers.
///
/// Manages per-peer accounting state and implements the `BandwidthAccounting` trait.
/// Settlement operations are delegated to configured providers. With no providers,
/// this behaves as a simple balance tracker.
///
/// # Type Parameters
///
/// - `C`: Configuration type implementing [`SwarmAccountingConfig`]
/// - `I`: Identity type implementing [`SwarmIdentity`]
///
/// # Example
///
/// ```ignore
/// use vertex_bandwidth_core::Accounting;
/// use vertex_swarm_api::DefaultAccountingConfig;
///
/// // Basic accounting (no settlement providers)
/// let accounting = Accounting::new(config, identity);
///
/// // With settlement providers
/// let accounting = Accounting::with_providers(
///     config,
///     identity,
///     vec![Box::new(my_provider)],
/// );
///
/// // Get a handle for a peer
/// let handle = accounting.for_peer(peer_address);
/// handle.record(1000, Direction::Download);
/// ```
pub struct Accounting<C, I: SwarmIdentity> {
    config: C,
    identity: I,
    providers: Arc<[Box<dyn SettlementProvider>]>,
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
        providers: Vec<Box<dyn SettlementProvider>>,
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
    pub fn providers(&self) -> &[Box<dyn SettlementProvider>] {
        &self.providers
    }

    /// Returns the names of the active settlement providers.
    pub fn provider_names(&self) -> Vec<&str> {
        self.providers.iter().map(|p| p.name()).collect()
    }

    /// Prepare a credit action (we are receiving service, balance decreases).
    pub fn prepare_credit(
        &self,
        peer: OverlayAddress,
        price: u64,
        _originated: bool,
    ) -> Result<CreditAction, AccountingError> {
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
        Ok(CreditAction::new(state, price))
    }

    /// Prepare a debit action (we are providing service, balance increases).
    pub fn prepare_debit(
        &self,
        peer: OverlayAddress,
        price: u64,
    ) -> Result<DebitAction, AccountingError> {
        let state = self.get_or_create_peer(peer);
        state.add_shadow_reserved(price);
        Ok(DebitAction::new(state, price))
    }

    /// Get or create peer state.
    ///
    /// Uses double-checked locking to minimize contention.
    pub fn get_or_create_peer(&self, peer: OverlayAddress) -> Arc<PeerState> {
        // Fast path: read lock
        {
            let peers = self.peers.read();
            if let Some(state) = peers.get(&peer) {
                return Arc::clone(state);
            }
        }

        // Slow path: write lock
        let mut peers = self.peers.write();
        peers
            .entry(peer)
            .or_insert_with(|| {
                Arc::new(PeerState::new(
                    peer,
                    self.config.payment_threshold(),
                    self.config.disconnect_threshold(),
                ))
            })
            .clone()
    }

    /// Get or create peer state for a light node.
    pub fn get_or_create_light_peer(&self, peer: OverlayAddress) -> Arc<PeerState> {
        // Fast path: read lock
        {
            let peers = self.peers.read();
            if let Some(state) = peers.get(&peer) {
                return Arc::clone(state);
            }
        }

        // Slow path: write lock
        let mut peers = self.peers.write();
        peers
            .entry(peer)
            .or_insert_with(|| {
                Arc::new(PeerState::new_light(
                    peer,
                    self.config.payment_threshold(),
                    self.config.disconnect_threshold(),
                    self.config.light_factor(),
                ))
            })
            .clone()
    }
}

impl<C: SwarmAccountingConfig, I: SwarmIdentity> SwarmBandwidthAccounting for Accounting<C, I> {
    type Identity = I;
    type Peer = AccountingPeerHandle;

    fn identity(&self) -> &I {
        &self.identity
    }

    fn for_peer(&self, peer: OverlayAddress) -> Self::Peer {
        let state = self.get_or_create_peer(peer);
        AccountingPeerHandle {
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
}

/// Handle to a peer's accounting state.
///
/// This handle is returned by [`Accounting::for_peer()`] and provides
/// the [`SwarmPeerBandwidth`] interface for recording bandwidth and checking balances.
///
/// Handles are cheap to clone (Arc references) and can be shared across
/// protocol handlers.
pub struct AccountingPeerHandle {
    state: Arc<PeerState>,
    providers: Arc<[Box<dyn SettlementProvider>]>,
    disconnect_threshold: u64,
    payment_threshold: u64,
}

impl Clone for AccountingPeerHandle {
    fn clone(&self) -> Self {
        Self {
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
            .map(|p| p.pre_allow(&self.state))
            .sum()
    }

    /// Call `settle()` on providers in order until debt is below threshold.
    async fn settle_all(&self) -> Result<i64, AccountingError> {
        let mut total = 0i64;

        for provider in self.providers.iter() {
            total = total.saturating_add(provider.settle(&self.state).await?);

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
        self.settle_all()
            .await
            .map(|_| ())
            .map_err(|e| SwarmError::PaymentRequired {
                reason: e.to_string(),
            })
    }

    fn peer(&self) -> OverlayAddress {
        self.state.peer()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settlement::NoSettlement;
    use vertex_swarm_api::{DefaultAccountingConfig, SwarmNodeType};
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
    fn test_prepare_credit() {
        let accounting = test_accounting();

        let action = accounting
            .prepare_credit(test_peer(), 1000, true)
            .expect("should prepare credit");

        let handle = accounting.for_peer(test_peer());
        assert_eq!(handle.state.reserved_balance(), 1000);

        action.apply();

        assert_eq!(handle.balance(), -1000);
        assert_eq!(handle.state.reserved_balance(), 0);
    }

    #[test]
    fn test_prepare_credit_dropped() {
        let accounting = test_accounting();

        {
            let _action = accounting
                .prepare_credit(test_peer(), 1000, true)
                .expect("should prepare credit");
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
    impl SettlementProvider for FixedAdjustProvider {
        fn pre_allow(&self, state: &PeerState) -> i64 {
            state.add_balance(self.0);
            self.0
        }

        async fn settle(&self, _state: &PeerState) -> Result<i64, AccountingError> {
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
            vec![Box::new(FixedAdjustProvider(100)), Box::new(FixedAdjustProvider(200))],
        );

        let handle = accounting.for_peer(test_peer());

        // Trigger pre_allow via allow()
        handle.allow(0);

        // Both providers should have adjusted the balance
        assert_eq!(handle.balance(), 300);
    }
}
