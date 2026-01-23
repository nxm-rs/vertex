//! Per-peer accounting for data availability.
//!
//! This module provides the core accounting infrastructure:
//!
//! - **PeerState**: Atomic per-peer balance tracking
//! - **Accounting**: Factory implementing `AvailabilityAccounting` trait
//! - **Actions**: `CreditAction` and `DebitAction` for prepare/apply pattern
//!
//! Settlement logic (pseudosettle, swap) is in separate crates.
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
//! chunk_price_au = (MAX_PO - proximity + 1) × base_price
//! ```
//!
//! Where:
//! - `MAX_PO = 31` (maximum proximity order, the bit-depth of addresses)
//! - `proximity` = number of leading bits two addresses share (0-31)
//! - `base_price = 10,000 AU` (Bee default)
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
//! The settlement mechanism is separate from accounting - this module only
//! tracks the AU balances.
//!
//! # Bee Compatibility
//!
//! All default values match the official Bee implementation to ensure
//! interoperability between Vertex and Bee nodes.

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
use vertex_swarm_api::{AvailabilityAccounting, Direction, PeerAvailability, SwarmResult};

// ============================================================================
// Bee-compatible accounting constants
// ============================================================================
//
// All values are in Accounting Units (AU). See module docs for details.
// These match Bee's values from bee/pkg/node/node.go.

/// Default base price per chunk in accounting units.
///
/// This is the price at maximum proximity (PO = 31).
/// Actual price scales with distance: `(MAX_PO - proximity + 1) × base_price`.
///
/// From Bee: `basePrice = 10_000`
pub const DEFAULT_BASE_PRICE: u64 = 10_000;

/// Default refresh rate for full nodes in accounting units per second.
///
/// In pseudosettle mode, this is the rate at which a peer's debt allowance
/// refreshes. A higher rate means more forgiving bandwidth accounting.
///
/// From Bee: `refreshRate = 4_500_000`
pub const DEFAULT_REFRESH_RATE: u64 = 4_500_000;

/// Default refresh rate for light nodes in accounting units per second.
///
/// Light nodes have reduced rates (1/10th of full nodes).
///
/// Calculated as: `DEFAULT_REFRESH_RATE / DEFAULT_LIGHT_FACTOR`
pub const DEFAULT_LIGHT_REFRESH_RATE: u64 = DEFAULT_REFRESH_RATE / DEFAULT_LIGHT_FACTOR;

/// Default payment threshold in accounting units.
///
/// When a peer's debt reaches this threshold, settlement is requested.
/// This is the point where we ask the peer to "pay up" their debt.
///
/// From Bee: `paymentThreshold = 13_500_000`
pub const DEFAULT_PAYMENT_THRESHOLD: u64 = 13_500_000;

/// Default payment tolerance as a percentage.
///
/// Adds a buffer above the payment threshold before disconnecting.
/// This prevents spurious disconnections due to race conditions.
///
/// From Bee: `paymentTolerance = 25%`
pub const DEFAULT_PAYMENT_TOLERANCE_PERCENT: u64 = 25;

/// Default early payment percentage.
///
/// Settlement is triggered when debt exceeds `(100 - early)%` of threshold.
/// With 50%, settlement triggers at 50% of the payment threshold.
///
/// From Bee: `paymentEarly = 50%`
pub const DEFAULT_EARLY_PAYMENT_PERCENT: u64 = 50;

/// Light node scaling factor.
///
/// Light nodes have all thresholds and rates divided by this factor,
/// making them more sensitive to bandwidth usage.
///
/// From Bee: `lightFactor = 10`
pub const DEFAULT_LIGHT_FACTOR: u64 = 10;

/// Thresholds and configuration for accounting.
///
/// All values are in **accounting units (AU)**, not bytes or BZZ tokens.
/// This matches Bee's accounting system.
#[derive(Debug, Clone)]
pub struct AccountingConfig {
    /// Payment threshold in accounting units.
    ///
    /// When a peer's debt reaches this threshold, settlement is requested.
    pub payment_threshold: u64,
    /// Payment tolerance as a percentage (0-100).
    ///
    /// Disconnect threshold = payment_threshold * (100 + tolerance) / 100
    pub payment_tolerance_percent: u64,
    /// Disconnect threshold in accounting units.
    ///
    /// When debt exceeds this, the connection is dropped.
    /// Calculated as: payment_threshold * (100 + tolerance) / 100
    pub disconnect_threshold: u64,
    /// Factor for light node thresholds.
    ///
    /// Light nodes have all thresholds divided by this factor.
    pub light_factor: u64,
    /// Base price per chunk in accounting units.
    ///
    /// This is the minimum price at maximum proximity.
    /// Actual price = (MAX_PO - proximity + 1) * base_price
    pub base_price: u64,
    /// Refresh rate in accounting units per second (for pseudosettle).
    pub refresh_rate: u64,
    /// Early payment threshold percentage.
    ///
    /// Settlement is triggered when debt exceeds (100 - early) % of threshold.
    pub early_payment_percent: u64,
}

impl Default for AccountingConfig {
    fn default() -> Self {
        let payment_threshold = DEFAULT_PAYMENT_THRESHOLD;
        let tolerance = DEFAULT_PAYMENT_TOLERANCE_PERCENT;
        // disconnect_threshold = threshold * (100 + tolerance) / 100
        let disconnect_threshold = payment_threshold * (100 + tolerance) / 100;

        Self {
            payment_threshold,
            payment_tolerance_percent: tolerance,
            disconnect_threshold,
            light_factor: DEFAULT_LIGHT_FACTOR,
            base_price: DEFAULT_BASE_PRICE,
            refresh_rate: DEFAULT_REFRESH_RATE,
            early_payment_percent: DEFAULT_EARLY_PAYMENT_PERCENT,
        }
    }
}

impl AccountingConfig {
    /// Create a configuration for light nodes.
    ///
    /// All thresholds and rates are divided by the light factor.
    pub fn light_node() -> Self {
        let full = Self::default();
        Self {
            payment_threshold: full.payment_threshold / full.light_factor,
            payment_tolerance_percent: full.payment_tolerance_percent,
            disconnect_threshold: full.disconnect_threshold / full.light_factor,
            light_factor: full.light_factor,
            base_price: full.base_price,
            refresh_rate: full.refresh_rate / full.light_factor,
            early_payment_percent: full.early_payment_percent,
        }
    }

    /// Calculate the early payment threshold.
    ///
    /// Settlement should be triggered when debt exceeds this.
    pub fn early_payment_threshold(&self) -> u64 {
        self.payment_threshold * (100 - self.early_payment_percent) / 100
    }

    /// Calculate the minimum payment amount for monetary settlement.
    ///
    /// From Bee: minimumPayment = refreshRate / 5
    pub fn minimum_payment(&self) -> u64 {
        self.refresh_rate / 5
    }
}

/// Core accounting implementation.
///
/// Manages per-peer accounting state and implements the `AvailabilityAccounting` trait.
/// This is the base accounting without settlement - use `PseudosettleAccounting` or
/// `SwapAccounting` for settlement capabilities.
pub struct Accounting {
    config: AccountingConfig,
    peers: RwLock<HashMap<OverlayAddress, Arc<PeerState>>>,
}

impl Accounting {
    /// Create a new accounting instance with the given configuration.
    pub fn new(config: AccountingConfig) -> Self {
        Self {
            config,
            peers: RwLock::new(HashMap::new()),
        }
    }

    /// Get accounting configuration.
    pub fn config(&self) -> &AccountingConfig {
        &self.config
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

        let threshold = -(self.config.disconnect_threshold as i64);
        if projected < threshold {
            return Err(AccountingError::DisconnectThreshold {
                peer,
                balance: current_balance,
                threshold: self.config.disconnect_threshold,
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
    pub fn get_or_create_peer(&self, peer: OverlayAddress) -> Arc<PeerState> {
        {
            let peers = self.peers.read();
            if let Some(state) = peers.get(&peer) {
                return Arc::clone(state);
            }
        }

        let mut peers = self.peers.write();
        peers
            .entry(peer)
            .or_insert_with(|| {
                Arc::new(PeerState::new(
                    peer,
                    self.config.payment_threshold,
                    self.config.disconnect_threshold,
                ))
            })
            .clone()
    }
}

impl AvailabilityAccounting for Accounting {
    type Peer = AccountingPeerHandle;

    fn for_peer(&self, peer: OverlayAddress) -> Self::Peer {
        let state = self.get_or_create_peer(peer);
        AccountingPeerHandle {
            state,
            config: self.config.clone(),
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
#[derive(Clone)]
pub struct AccountingPeerHandle {
    state: Arc<PeerState>,
    config: AccountingConfig,
}

impl AccountingPeerHandle {
    /// Get access to the underlying peer state.
    pub fn state(&self) -> &Arc<PeerState> {
        &self.state
    }
}

#[async_trait::async_trait]
impl PeerAvailability for AccountingPeerHandle {
    fn record(&self, bytes: u64, direction: Direction) {
        match direction {
            Direction::Upload => self.state.add_balance(bytes as i64),
            Direction::Download => self.state.add_balance(-(bytes as i64)),
        }
    }

    fn allow(&self, bytes: u64) -> bool {
        let balance = self.state.balance();
        let reserved = self.state.reserved_balance();
        let projected = balance - (bytes as i64) - (reserved as i64);
        projected >= -(self.config.disconnect_threshold as i64)
    }

    fn balance(&self) -> i64 {
        self.state.balance()
    }

    async fn settle(&self) -> SwarmResult<()> {
        Ok(())
    }

    fn peer(&self) -> OverlayAddress {
        self.state.peer()
    }
}

// ============================================================================
// Default Configuration Implementations
// ============================================================================

use vertex_swarm_api::AvailabilityIncentiveConfig;

/// Default availability configuration (pseudosettle only, Bee-compatible thresholds).
///
/// This provides sensible defaults for a full node running pseudosettle:
/// - Payment threshold: 13,500,000 AU
/// - Tolerance: 25%
/// - Base price: 10,000 AU
/// - Refresh rate: 4,500,000 AU/s
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultAvailabilityConfig;

impl AvailabilityIncentiveConfig for DefaultAvailabilityConfig {
    fn pseudosettle_enabled(&self) -> bool {
        true
    }

    fn swap_enabled(&self) -> bool {
        false
    }

    fn payment_threshold(&self) -> u64 {
        DEFAULT_PAYMENT_THRESHOLD
    }

    fn payment_tolerance_percent(&self) -> u64 {
        DEFAULT_PAYMENT_TOLERANCE_PERCENT
    }

    fn base_price(&self) -> u64 {
        DEFAULT_BASE_PRICE
    }

    fn refresh_rate(&self) -> u64 {
        DEFAULT_REFRESH_RATE
    }

    fn early_payment_percent(&self) -> u64 {
        DEFAULT_EARLY_PAYMENT_PERCENT
    }

    fn light_factor(&self) -> u64 {
        DEFAULT_LIGHT_FACTOR
    }
}

/// No availability incentives configuration.
///
/// Use this when running without availability accounting (dev/testing only).
/// All thresholds are disabled and disconnect never happens.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoAvailabilityConfig;

impl AvailabilityIncentiveConfig for NoAvailabilityConfig {
    fn pseudosettle_enabled(&self) -> bool {
        false
    }

    fn swap_enabled(&self) -> bool {
        false
    }

    fn payment_threshold(&self) -> u64 {
        0
    }

    fn payment_tolerance_percent(&self) -> u64 {
        0
    }

    fn base_price(&self) -> u64 {
        0
    }

    fn refresh_rate(&self) -> u64 {
        0
    }

    fn early_payment_percent(&self) -> u64 {
        0
    }

    fn light_factor(&self) -> u64 {
        DEFAULT_LIGHT_FACTOR
    }

    fn disconnect_threshold(&self) -> u64 {
        u64::MAX // Never disconnect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_peer() -> OverlayAddress {
        OverlayAddress::from([1u8; 32])
    }

    #[test]
    fn test_accounting_basic() {
        let accounting = Accounting::new(AccountingConfig::default());

        let handle = accounting.for_peer(test_peer());
        assert_eq!(handle.balance(), 0);

        handle.record(1000, Direction::Upload);
        assert_eq!(handle.balance(), 1000);

        handle.record(500, Direction::Download);
        assert_eq!(handle.balance(), 500);
    }

    #[test]
    fn test_prepare_credit() {
        let accounting = Accounting::new(AccountingConfig::default());

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
        let accounting = Accounting::new(AccountingConfig::default());

        {
            let _action = accounting
                .prepare_credit(test_peer(), 1000, true)
                .expect("should prepare credit");
        }

        let handle = accounting.for_peer(test_peer());
        assert_eq!(handle.balance(), 0);
        assert_eq!(handle.state.reserved_balance(), 0);
    }
}
