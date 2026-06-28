//! Atomic per-peer balance tracking for lock-free bandwidth recording.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

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

/// Atomic per-peer balance state.
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
    payment_threshold: Au,
    disconnect_threshold: Au,
}

impl PeerState {
    /// Create peer state with the given thresholds in AU.
    pub fn new(payment_threshold: Au, disconnect_threshold: Au) -> Self {
        Self {
            balance: AtomicI64::new(0),
            reserved_balance: AtomicU64::new(0),
            shadow_reserved_balance: AtomicU64::new(0),
            payment_threshold,
            disconnect_threshold,
        }
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

    /// Get the payment threshold in AU.
    pub fn payment_threshold(&self) -> Au {
        self.payment_threshold
    }

    /// Get the disconnect threshold in AU.
    pub fn disconnect_threshold(&self) -> Au {
        self.disconnect_threshold
    }
}

impl SwarmPeerState for PeerState {
    fn balance(&self) -> Au {
        Au::new(self.balance.load(Ordering::Relaxed))
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
    }

    #[test]
    fn test_add_balance_saturates_instead_of_wrapping() {
        // Adding into the positive bound saturates rather than wrapping to a
        // negative balance (which would flip owed/owes).
        let state = PeerState::new(au(1000), au(10000));
        state.add_balance(Au::new(i64::MAX));
        state.add_balance(au(1000));
        assert_eq!(state.balance(), Au::new(i64::MAX));

        // The negative bound saturates too.
        let state = PeerState::new(au(1000), au(10000));
        state.add_balance(Au::new(i64::MIN));
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
    fn test_thresholds() {
        let state = PeerState::new(au(1000), au(10000));

        assert_eq!(state.payment_threshold(), au(1000));
        assert_eq!(state.disconnect_threshold(), au(10000));
    }
}
