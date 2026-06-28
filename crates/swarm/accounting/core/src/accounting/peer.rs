//! Atomic per-peer balance tracking for lock-free bandwidth recording.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use vertex_swarm_api::{Au, SwarmPeerState};

/// Add `delta` to an atomic balance, saturating at the [`i64`] bounds.
///
/// Plain `fetch_add` wraps on overflow and could flip a balance's sign,
/// inverting owed/owes; a compare-exchange loop saturates instead.
fn saturating_fetch_add(atomic: &AtomicI64, delta: i64) {
    let mut current = atomic.load(Ordering::Relaxed);
    loop {
        let next = current.saturating_add(delta);
        match atomic.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

/// Subtract `delta` from an unsigned atomic reserve, saturating at zero.
///
/// Plain `fetch_sub` wraps to near `u64::MAX` on underflow, which readers clamp
/// to `i64::MAX` and subtract from every allowance, jamming the peer into
/// permanent denial; a compare-exchange loop floors a mismatched release at zero.
fn saturating_fetch_sub(atomic: &AtomicU64, delta: u64) {
    let mut current = atomic.load(Ordering::Relaxed);
    loop {
        let next = current.saturating_sub(delta);
        match atomic.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

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
    payment_threshold: Au,
    disconnect_threshold: Au,
    last_refresh: AtomicU64,
}

impl PeerState {
    /// Create peer state with the given thresholds in AU.
    pub fn new(payment_threshold: Au, disconnect_threshold: Au) -> Self {
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
    pub fn new_client_only(payment_threshold: Au, disconnect_threshold: Au, factor: u64) -> Self {
        let factor = factor.max(1);
        Self::new(
            Au::from_amount(payment_threshold.as_amount() / factor),
            Au::from_amount(disconnect_threshold.as_amount() / factor),
        )
    }

    /// Get the current balance in AU.
    pub fn balance(&self) -> Au {
        Au::new(self.balance.load(Ordering::Relaxed))
    }

    /// Add to the balance atomically, saturating at the [`i64`] bounds so an
    /// adversarial price or settlement sequence cannot wrap and flip owed/owes.
    pub fn add_balance(&self, amount: Au) {
        saturating_fetch_add(&self.balance, amount.get());
    }

    /// Set the balance atomically.
    pub fn set_balance(&self, amount: Au) {
        self.balance.store(amount.get(), Ordering::Relaxed);
    }

    /// Get the reserved balance in AU.
    pub fn reserved_balance(&self) -> Au {
        Au::from_amount(self.reserved_balance.load(Ordering::Relaxed))
    }

    /// Add to reserved balance.
    pub fn add_reserved(&self, amount: Au) {
        self.reserved_balance
            .fetch_add(amount.as_amount(), Ordering::Relaxed);
    }

    /// Subtract from reserved balance, saturating at zero.
    pub fn sub_reserved(&self, amount: Au) {
        saturating_fetch_sub(&self.reserved_balance, amount.as_amount());
    }

    /// Get the shadow reserved balance in AU.
    pub fn shadow_reserved_balance(&self) -> Au {
        Au::from_amount(self.shadow_reserved_balance.load(Ordering::Relaxed))
    }

    /// Add to shadow reserved balance.
    pub fn add_shadow_reserved(&self, amount: Au) {
        self.shadow_reserved_balance
            .fetch_add(amount.as_amount(), Ordering::Relaxed);
    }

    /// Subtract from shadow reserved balance, saturating at zero.
    pub fn sub_shadow_reserved(&self, amount: Au) {
        saturating_fetch_sub(&self.shadow_reserved_balance, amount.as_amount());
    }

    /// Get the surplus balance in AU.
    pub fn surplus_balance(&self) -> Au {
        Au::new(self.surplus_balance.load(Ordering::Relaxed))
    }

    /// Add to surplus balance, saturating at the [`i64`] bounds.
    pub fn add_surplus(&self, amount: Au) {
        saturating_fetch_add(&self.surplus_balance, amount.get());
    }

    /// Get the payment threshold in AU.
    pub fn payment_threshold(&self) -> Au {
        self.payment_threshold
    }

    /// Get the disconnect threshold in AU.
    pub fn disconnect_threshold(&self) -> Au {
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
    fn balance(&self) -> Au {
        Au::new(self.balance.load(Ordering::Relaxed))
    }

    fn add_balance(&self, amount: Au) {
        saturating_fetch_add(&self.balance, amount.get());
    }

    fn last_refresh(&self) -> u64 {
        self.last_refresh.load(Ordering::Relaxed)
    }

    fn set_last_refresh(&self, timestamp: u64) {
        self.last_refresh.store(timestamp, Ordering::Relaxed);
    }

    fn payment_threshold(&self) -> Au {
        self.payment_threshold
    }

    fn disconnect_threshold(&self) -> Au {
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
    ///
    /// The snapshot stores plain integers so the persisted bytes are
    /// unchanged by the AU typing.
    pub fn snapshot(&self) -> PeerStateSnapshot {
        PeerStateSnapshot {
            balance: self.balance.load(Ordering::Relaxed),
            surplus_balance: self.surplus_balance.load(Ordering::Relaxed),
            payment_threshold: self.payment_threshold.as_amount(),
            disconnect_threshold: self.disconnect_threshold.as_amount(),
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
            payment_threshold: Au::from_amount(snapshot.payment_threshold),
            disconnect_threshold: Au::from_amount(snapshot.disconnect_threshold),
            last_refresh: AtomicU64::new(snapshot.last_refresh),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn au(value: i64) -> Au {
        Au::new(value)
    }

    #[test]
    fn test_balance_operations() {
        let state = PeerState::new(au(1000), au(10000));

        assert_eq!(state.balance(), Au::ZERO);

        state.add_balance(au(100));
        assert_eq!(state.balance(), au(100));

        state.add_balance(au(-50));
        assert_eq!(state.balance(), au(50));

        state.set_balance(au(200));
        assert_eq!(state.balance(), au(200));
    }

    #[test]
    fn test_add_balance_saturates_instead_of_wrapping() {
        let state = PeerState::new(au(1000), au(10000));

        // Adding into the positive bound saturates rather than wrapping to a
        // negative balance (which would flip owed/owes).
        state.set_balance(Au::new(i64::MAX));
        state.add_balance(au(1000));
        assert_eq!(state.balance(), Au::new(i64::MAX));

        // The negative bound saturates too.
        state.set_balance(Au::new(i64::MIN));
        state.add_balance(au(-1000));
        assert_eq!(state.balance(), Au::new(i64::MIN));
    }

    #[test]
    fn test_reserved_operations() {
        let state = PeerState::new(au(1000), au(10000));

        assert_eq!(state.reserved_balance(), Au::ZERO);

        state.add_reserved(au(100));
        assert_eq!(state.reserved_balance(), au(100));

        state.sub_reserved(au(50));
        assert_eq!(state.reserved_balance(), au(50));
    }

    #[test]
    fn test_sub_reserved_saturates_at_zero() {
        let state = PeerState::new(au(1000), au(10000));

        // Releasing more than is reserved must saturate at zero, never wrap to
        // a near-u64::MAX reserve that would read back as i64::MAX and jam the
        // peer into permanent denial.
        state.add_reserved(au(100));
        state.sub_reserved(au(250));
        assert_eq!(state.reserved_balance(), Au::ZERO);

        state.add_shadow_reserved(au(100));
        state.sub_shadow_reserved(au(250));
        assert_eq!(state.shadow_reserved_balance(), Au::ZERO);
    }

    #[test]
    fn test_client_node_thresholds() {
        let state = PeerState::new_client_only(au(1000), au(10000), 5);

        // Thresholds should be scaled down by client_factor
        assert_eq!(state.payment_threshold(), au(200));
        assert_eq!(state.disconnect_threshold(), au(2000));
    }

    #[test]
    fn test_snapshot_roundtrip() {
        let state = PeerState::new(au(1000), au(10000));
        state.add_balance(au(500));
        state.add_surplus(au(100));
        state.set_last_refresh(12345);

        let snapshot = state.snapshot();
        let restored = PeerState::from_snapshot(snapshot);

        assert_eq!(restored.balance(), au(500));
        assert_eq!(restored.surplus_balance(), au(100));
        assert_eq!(restored.last_refresh(), 12345);
        assert_eq!(restored.payment_threshold(), au(1000));
        assert_eq!(restored.disconnect_threshold(), au(10000));
    }
}
