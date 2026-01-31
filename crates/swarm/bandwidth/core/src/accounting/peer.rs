//! Atomic per-peer balance tracking for lock-free bandwidth recording.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use vertex_swarm_api::SwarmPeerState;

/// Atomic per-peer balance state (serializable for persistence).
///
/// - Positive balance: peer owes us (we provided service)
/// - Negative balance: we owe peer (we received service)
///
/// The peer address and node type are not stored; address is the map key,
/// node type can be looked up via the peer manager.
pub struct PeerState {
    balance: AtomicI64,
    reserved_balance: AtomicU64,
    shadow_reserved_balance: AtomicU64,
    surplus_balance: AtomicI64,
    payment_threshold: u64,
    disconnect_threshold: u64,
    last_refresh: AtomicU64,
}

impl PeerState {
    /// Create peer state with the given thresholds.
    pub fn new(payment_threshold: u64, disconnect_threshold: u64) -> Self {
        Self {
            balance: AtomicI64::new(0),
            reserved_balance: AtomicU64::new(0),
            shadow_reserved_balance: AtomicU64::new(0),
            surplus_balance: AtomicI64::new(0),
            payment_threshold,
            disconnect_threshold,
            last_refresh: AtomicU64::new(0),
        }
    }

    /// Create peer state with scaled thresholds for a client-only node.
    pub fn new_client_only(payment_threshold: u64, disconnect_threshold: u64, factor: u64) -> Self {
        Self::new(payment_threshold / factor, disconnect_threshold / factor)
    }

    /// Get the current balance.
    pub fn balance(&self) -> i64 {
        self.balance.load(Ordering::Relaxed)
    }

    /// Add to the balance atomically.
    pub fn add_balance(&self, amount: i64) {
        self.balance.fetch_add(amount, Ordering::Relaxed);
    }

    /// Set the balance atomically.
    pub fn set_balance(&self, amount: i64) {
        self.balance.store(amount, Ordering::Relaxed);
    }

    /// Get the reserved balance.
    pub fn reserved_balance(&self) -> u64 {
        self.reserved_balance.load(Ordering::Relaxed)
    }

    /// Add to reserved balance.
    pub fn add_reserved(&self, amount: u64) {
        self.reserved_balance.fetch_add(amount, Ordering::Relaxed);
    }

    /// Subtract from reserved balance.
    pub fn sub_reserved(&self, amount: u64) {
        self.reserved_balance.fetch_sub(amount, Ordering::Relaxed);
    }

    /// Get the shadow reserved balance.
    pub fn shadow_reserved_balance(&self) -> u64 {
        self.shadow_reserved_balance.load(Ordering::Relaxed)
    }

    /// Add to shadow reserved balance.
    pub fn add_shadow_reserved(&self, amount: u64) {
        self.shadow_reserved_balance
            .fetch_add(amount, Ordering::Relaxed);
    }

    /// Subtract from shadow reserved balance.
    pub fn sub_shadow_reserved(&self, amount: u64) {
        self.shadow_reserved_balance
            .fetch_sub(amount, Ordering::Relaxed);
    }

    /// Get the surplus balance.
    pub fn surplus_balance(&self) -> i64 {
        self.surplus_balance.load(Ordering::Relaxed)
    }

    /// Add to surplus balance.
    pub fn add_surplus(&self, amount: i64) {
        self.surplus_balance.fetch_add(amount, Ordering::Relaxed);
    }

    /// Get the payment threshold.
    pub fn payment_threshold(&self) -> u64 {
        self.payment_threshold
    }

    /// Get the disconnect threshold.
    pub fn disconnect_threshold(&self) -> u64 {
        self.disconnect_threshold
    }

    /// Get the last refresh timestamp.
    pub fn last_refresh(&self) -> u64 {
        self.last_refresh.load(Ordering::Relaxed)
    }

    /// Set the last refresh timestamp.
    pub fn set_last_refresh(&self, timestamp: u64) {
        self.last_refresh.store(timestamp, Ordering::Relaxed);
    }
}

impl SwarmPeerState for PeerState {
    fn balance(&self) -> i64 {
        self.balance.load(Ordering::Relaxed)
    }

    fn add_balance(&self, amount: i64) {
        self.balance.fetch_add(amount, Ordering::Relaxed);
    }

    fn last_refresh(&self) -> u64 {
        self.last_refresh.load(Ordering::Relaxed)
    }

    fn set_last_refresh(&self, timestamp: u64) {
        self.last_refresh.store(timestamp, Ordering::Relaxed);
    }

    fn payment_threshold(&self) -> u64 {
        self.payment_threshold
    }

    fn disconnect_threshold(&self) -> u64 {
        self.disconnect_threshold
    }
}

/// Snapshot of peer state for serialization/persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerStateSnapshot {
    pub balance: i64,
    pub surplus_balance: i64,
    pub payment_threshold: u64,
    pub disconnect_threshold: u64,
    pub last_refresh: u64,
}

impl PeerState {
    /// Create a snapshot for persistence.
    pub fn snapshot(&self) -> PeerStateSnapshot {
        PeerStateSnapshot {
            balance: self.balance.load(Ordering::Relaxed),
            surplus_balance: self.surplus_balance.load(Ordering::Relaxed),
            payment_threshold: self.payment_threshold,
            disconnect_threshold: self.disconnect_threshold,
            last_refresh: self.last_refresh.load(Ordering::Relaxed),
        }
    }

    /// Restore from a snapshot.
    pub fn from_snapshot(snapshot: PeerStateSnapshot) -> Self {
        Self {
            balance: AtomicI64::new(snapshot.balance),
            reserved_balance: AtomicU64::new(0),
            shadow_reserved_balance: AtomicU64::new(0),
            surplus_balance: AtomicI64::new(snapshot.surplus_balance),
            payment_threshold: snapshot.payment_threshold,
            disconnect_threshold: snapshot.disconnect_threshold,
            last_refresh: AtomicU64::new(snapshot.last_refresh),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_balance_operations() {
        let state = PeerState::new(1000, 10000);

        assert_eq!(state.balance(), 0);

        state.add_balance(100);
        assert_eq!(state.balance(), 100);

        state.add_balance(-50);
        assert_eq!(state.balance(), 50);

        state.set_balance(200);
        assert_eq!(state.balance(), 200);
    }

    #[test]
    fn test_reserved_operations() {
        let state = PeerState::new(1000, 10000);

        assert_eq!(state.reserved_balance(), 0);

        state.add_reserved(100);
        assert_eq!(state.reserved_balance(), 100);

        state.sub_reserved(50);
        assert_eq!(state.reserved_balance(), 50);
    }

    #[test]
    fn test_client_node_thresholds() {
        let state = PeerState::new_client_only(1000, 10000, 5);

        // Thresholds should be scaled down by client_factor
        assert_eq!(state.payment_threshold(), 200);
        assert_eq!(state.disconnect_threshold(), 2000);
    }

    #[test]
    fn test_snapshot_roundtrip() {
        let state = PeerState::new(1000, 10000);
        state.add_balance(500);
        state.add_surplus(100);
        state.set_last_refresh(12345);

        let snapshot = state.snapshot();
        let restored = PeerState::from_snapshot(snapshot);

        assert_eq!(restored.balance(), 500);
        assert_eq!(restored.surplus_balance(), 100);
        assert_eq!(restored.last_refresh(), 12345);
        assert_eq!(restored.payment_threshold(), 1000);
        assert_eq!(restored.disconnect_threshold(), 10000);
    }
}
