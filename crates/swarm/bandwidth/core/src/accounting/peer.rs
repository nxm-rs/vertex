//! Per-peer accounting state.
//!
//! Each peer has atomic counters for balance tracking, enabling lock-free
//! recording of bandwidth usage from multiple protocol handlers.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use vertex_primitives::OverlayAddress;

/// Per-peer accounting state.
///
/// Uses atomic operations for all balance modifications, allowing concurrent
/// access from multiple protocol handlers without locks.
///
/// # Balance Semantics
///
/// - **Positive balance**: Peer owes us (we provided service)
/// - **Negative balance**: We owe peer (we received service)
///
/// # Reserved Balances
///
/// - `reserved_balance`: Our pending local operations (credit)
/// - `shadow_reserved_balance`: Expected incoming from peer (debit)
/// - `surplus_balance`: Received payments not yet settled
pub struct PeerState {
    peer: OverlayAddress,
    balance: AtomicI64,
    reserved_balance: AtomicU64,
    shadow_reserved_balance: AtomicU64,
    surplus_balance: AtomicI64,
    full_node: bool,
    payment_threshold: u64,
    disconnect_threshold: u64,
    last_refresh: AtomicU64,
}

impl PeerState {
    /// Create a new peer state.
    pub fn new(peer: OverlayAddress, payment_threshold: u64, disconnect_threshold: u64) -> Self {
        Self {
            peer,
            balance: AtomicI64::new(0),
            reserved_balance: AtomicU64::new(0),
            shadow_reserved_balance: AtomicU64::new(0),
            surplus_balance: AtomicI64::new(0),
            full_node: true,
            payment_threshold,
            disconnect_threshold,
            last_refresh: AtomicU64::new(0),
        }
    }

    /// Create a new peer state for a light node.
    pub fn new_light(
        peer: OverlayAddress,
        payment_threshold: u64,
        disconnect_threshold: u64,
        light_factor: u64,
    ) -> Self {
        Self {
            peer,
            balance: AtomicI64::new(0),
            reserved_balance: AtomicU64::new(0),
            shadow_reserved_balance: AtomicU64::new(0),
            surplus_balance: AtomicI64::new(0),
            full_node: false,
            payment_threshold: payment_threshold / light_factor,
            disconnect_threshold: disconnect_threshold / light_factor,
            last_refresh: AtomicU64::new(0),
        }
    }

    /// Get the peer's overlay address.
    pub fn peer(&self) -> OverlayAddress {
        self.peer
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

    /// Whether this peer is a full node.
    pub fn is_full_node(&self) -> bool {
        self.full_node
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_peer() -> OverlayAddress {
        OverlayAddress::from([1u8; 32])
    }

    #[test]
    fn test_balance_operations() {
        let state = PeerState::new(test_peer(), 1000, 10000);

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
        let state = PeerState::new(test_peer(), 1000, 10000);

        assert_eq!(state.reserved_balance(), 0);

        state.add_reserved(100);
        assert_eq!(state.reserved_balance(), 100);

        state.sub_reserved(50);
        assert_eq!(state.reserved_balance(), 50);
    }

    #[test]
    fn test_light_node_thresholds() {
        let state = PeerState::new_light(test_peer(), 1000, 10000, 5);

        assert!(!state.is_full_node());
        assert_eq!(state.payment_threshold(), 200);
        assert_eq!(state.disconnect_threshold(), 2000);
    }
}
