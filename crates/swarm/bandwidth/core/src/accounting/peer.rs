//! Per-peer balance tracking for lock-free bandwidth recording.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use vertex_swarm_api::SwarmPeerAccounting;

/// Multiplier for the linear step size in credit limit growth.
/// `credit_limit_grow_at` starts at `refresh_rate * LINEAR_STEP_FACTOR`.
const LINEAR_STEP_FACTOR: u64 = 100;

/// Multiplier for the linear-to-exponential switch point.
/// Once `credit_limit_grow_at >= refresh_rate * LINEAR_CHECKPOINT_COUNT`,
/// subsequent growth is exponential (doubling).
const LINEAR_CHECKPOINT_COUNT: u64 = 1800;

/// Per-peer balance state (serialisable for persistence).
///
/// - Positive balance: peer owes us (we provided service)
/// - Negative balance: we owe peer (we received service)
///
/// The peer address and node type are not stored; address is the map key,
/// node type can be looked up via the peer manager.
///
/// Transient reservation counters are excluded from serialisation and reset
/// to zero on deserialisation. Trust ramp fields and the remote credit limit
/// are persisted so that earned trust survives restarts.
#[derive(Debug, Serialize, Deserialize)]
pub struct PeerAccounting {
    balance: AtomicI64,
    #[serde(skip)]
    reserved_balance: AtomicU64,
    #[serde(skip)]
    shadow_reserved_balance: AtomicU64,
    surplus_balance: AtomicI64,
    credit_limit: u64,
    disconnect_limit: u64,
    last_refresh: AtomicU64,
    /// Credit limit announced by the remote peer via the credit protocol.
    /// Persisted so that trust state survives restarts.
    remote_credit_limit: AtomicU64,

    // -- Trust ramp state --
    /// Our locally determined credit limit for this peer. Starts at the
    /// configured default and grows as the peer proves creditworthy
    /// through successful settlements.
    local_credit_limit: AtomicU64,
    /// Cumulative amount of debt repaid by this peer (via pseudosettle
    /// or swap). Used to decide when to grow the outbound credit limit.
    total_debt_repaid: AtomicU64,
    /// Next checkpoint at which the outbound credit limit will be
    /// increased. Grows linearly at first, then exponentially.
    credit_limit_grow_at: AtomicU64,
    /// Refresh rate used for trust ramp step size. Stored per-peer
    /// because light nodes use a scaled-down value.
    refresh_rate: u64,
}

impl Clone for PeerAccounting {
    /// Clone by reading all fields. Transient reservation counters
    /// reset to zero; trust ramp state is preserved.
    fn clone(&self) -> Self {
        Self {
            balance: AtomicI64::new(self.balance.load(Ordering::Relaxed)),
            reserved_balance: AtomicU64::new(0),
            shadow_reserved_balance: AtomicU64::new(0),
            surplus_balance: AtomicI64::new(self.surplus_balance.load(Ordering::Relaxed)),
            credit_limit: self.credit_limit,
            disconnect_limit: self.disconnect_limit,
            last_refresh: AtomicU64::new(self.last_refresh.load(Ordering::Relaxed)),
            remote_credit_limit: AtomicU64::new(self.remote_credit_limit.load(Ordering::Relaxed)),
            local_credit_limit: AtomicU64::new(
                self.local_credit_limit.load(Ordering::Relaxed),
            ),
            total_debt_repaid: AtomicU64::new(self.total_debt_repaid.load(Ordering::Relaxed)),
            credit_limit_grow_at: AtomicU64::new(self.credit_limit_grow_at.load(Ordering::Relaxed)),
            refresh_rate: self.refresh_rate,
        }
    }
}

impl PeerAccounting {
    /// Create peer state with the given limits.
    ///
    /// `refresh_rate` is used to initialise the trust ramp checkpoints.
    pub fn new(credit_limit: u64, disconnect_limit: u64) -> Self {
        Self::with_refresh_rate(credit_limit, disconnect_limit, 0)
    }

    /// Create peer state with the given limits and refresh rate for trust ramp.
    pub fn with_refresh_rate(credit_limit: u64, disconnect_limit: u64, refresh_rate: u64) -> Self {
        Self {
            balance: AtomicI64::new(0),
            reserved_balance: AtomicU64::new(0),
            shadow_reserved_balance: AtomicU64::new(0),
            surplus_balance: AtomicI64::new(0),
            credit_limit,
            disconnect_limit,
            last_refresh: AtomicU64::new(0),
            remote_credit_limit: AtomicU64::new(0),
            local_credit_limit: AtomicU64::new(credit_limit),
            total_debt_repaid: AtomicU64::new(0),
            credit_limit_grow_at: AtomicU64::new(refresh_rate.saturating_mul(LINEAR_STEP_FACTOR)),
            refresh_rate,
        }
    }

    /// Create peer state with scaled limits for a client-only node.
    pub fn new_client_only(credit_limit: u64, disconnect_limit: u64, factor: u64) -> Self {
        Self::new(credit_limit / factor, disconnect_limit / factor)
    }

    /// Create peer state with scaled limits for a client-only node,
    /// including refresh rate for trust ramp initialisation.
    pub fn new_client_only_with_refresh(
        credit_limit: u64,
        disconnect_limit: u64,
        factor: u64,
        refresh_rate: u64,
    ) -> Self {
        let scaled_refresh = refresh_rate / factor;
        Self::with_refresh_rate(
            credit_limit / factor,
            disconnect_limit / factor,
            scaled_refresh,
        )
    }

    /// Get the current balance.
    pub fn balance(&self) -> i64 {
        self.balance.load(Ordering::Relaxed)
    }

    /// Add to the balance.
    pub fn add_balance(&self, amount: i64) {
        self.balance.fetch_add(amount, Ordering::Relaxed);
    }

    /// Set the balance.
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

    /// Get the credit limit.
    pub fn credit_limit(&self) -> u64 {
        self.credit_limit
    }

    /// Get the disconnect limit.
    pub fn disconnect_limit(&self) -> u64 {
        self.disconnect_limit
    }

    /// Get the last refresh timestamp.
    pub fn last_refresh(&self) -> u64 {
        self.last_refresh.load(Ordering::Relaxed)
    }

    /// Set the last refresh timestamp.
    pub fn set_last_refresh(&self, timestamp: u64) {
        self.last_refresh.store(timestamp, Ordering::Relaxed);
    }

    /// Get the credit limit announced by the remote peer.
    ///
    /// Returns zero if the peer has not yet announced their limit.
    pub fn remote_credit_limit(&self) -> u64 {
        self.remote_credit_limit.load(Ordering::Relaxed)
    }

    /// Record the credit limit announced by the remote peer.
    pub fn announce_credit_limit(&self, limit: u64) {
        self.remote_credit_limit.store(limit, Ordering::Relaxed);
    }

    // -- Trust ramp accessors --

    /// Get our locally determined credit limit for this peer.
    pub fn local_credit_limit(&self) -> u64 {
        self.local_credit_limit.load(Ordering::Relaxed)
    }

    /// Get the total debt repaid by this peer.
    pub fn total_debt_repaid(&self) -> u64 {
        self.total_debt_repaid.load(Ordering::Relaxed)
    }

    /// Get the next growth checkpoint.
    pub fn credit_limit_grow_at(&self) -> u64 {
        self.credit_limit_grow_at.load(Ordering::Relaxed)
    }

    /// Get the refresh rate used for trust ramp calculations.
    pub fn refresh_rate(&self) -> u64 {
        self.refresh_rate
    }

    /// Notify that a settlement was received from this peer.
    ///
    /// Implements the trust ramp algorithm: linear growth up to
    /// `LINEAR_CHECKPOINT_COUNT` steps, then exponential doubling.
    /// Returns `true` if the local credit limit was increased
    /// (caller should re-announce).
    pub fn notify_settlement_received(&self, amount: u64) -> bool {
        if self.refresh_rate == 0 {
            return false;
        }

        let new_total = self
            .total_debt_repaid
            .fetch_add(amount, Ordering::Relaxed)
            .saturating_add(amount);

        // CAS loop: only one concurrent caller advances the checkpoint.
        loop {
            let grow_at = self.credit_limit_grow_at.load(Ordering::Relaxed);
            if grow_at == 0 || new_total <= grow_at {
                return false;
            }

            // Compute the next checkpoint
            let linear_threshold = self.refresh_rate.saturating_mul(LINEAR_CHECKPOINT_COUNT);
            let new_grow_at = if grow_at >= linear_threshold {
                grow_at.saturating_mul(2) // exponential
            } else {
                grow_at.saturating_add(self.refresh_rate.saturating_mul(LINEAR_STEP_FACTOR)) // linear
            };

            // Attempt to claim this growth step
            if self
                .credit_limit_grow_at
                .compare_exchange(grow_at, new_grow_at, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                // Won the race -- bump the outbound credit limit
                self.local_credit_limit
                    .fetch_add(self.refresh_rate, Ordering::Relaxed);
                return true;
            }
            // Lost the race -- retry with the updated grow_at
        }
    }
}

impl SwarmPeerAccounting for PeerAccounting {
    fn balance(&self) -> i64 {
        PeerAccounting::balance(self)
    }

    fn add_balance(&self, amount: i64) {
        PeerAccounting::add_balance(self, amount);
    }

    fn last_refresh(&self) -> u64 {
        PeerAccounting::last_refresh(self)
    }

    fn record_refresh(&self, timestamp: u64) {
        PeerAccounting::set_last_refresh(self, timestamp);
    }

    fn credit_limit(&self) -> u64 {
        PeerAccounting::credit_limit(self)
    }

    fn disconnect_limit(&self) -> u64 {
        PeerAccounting::disconnect_limit(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_balance_operations() {
        let state = PeerAccounting::new(1000, 10000);

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
        let state = PeerAccounting::new(1000, 10000);

        assert_eq!(state.reserved_balance(), 0);

        state.add_reserved(100);
        assert_eq!(state.reserved_balance(), 100);

        state.sub_reserved(50);
        assert_eq!(state.reserved_balance(), 50);
    }

    #[test]
    fn test_client_node_thresholds() {
        let state = PeerAccounting::new_client_only(1000, 10000, 5);

        // Limits should be scaled down by client_factor
        assert_eq!(state.credit_limit(), 200);
        assert_eq!(state.disconnect_limit(), 2000);
    }

    #[test]
    fn test_serde_roundtrip() {
        let state = PeerAccounting::with_refresh_rate(1000, 10000, 100);
        state.add_balance(500);
        state.add_surplus(100);
        state.set_last_refresh(12345);
        state.announce_credit_limit(9999);

        let serialised = postcard::to_allocvec(&state).expect("serialise");
        let restored: PeerAccounting = postcard::from_bytes(&serialised).expect("deserialise");

        assert_eq!(restored.balance(), 500);
        assert_eq!(restored.surplus_balance(), 100);
        assert_eq!(restored.last_refresh(), 12345);
        assert_eq!(restored.credit_limit(), 1000);
        assert_eq!(restored.disconnect_limit(), 10000);
        // remote_credit_limit is now persisted
        assert_eq!(restored.remote_credit_limit(), 9999);
        // Trust ramp state is persisted
        assert_eq!(restored.local_credit_limit(), 1000);
        assert_eq!(restored.total_debt_repaid(), 0);
        assert_eq!(restored.credit_limit_grow_at(), 100 * 100); // refresh_rate * LINEAR_STEP_FACTOR
        // Transient fields reset to zero
        assert_eq!(restored.reserved_balance(), 0);
        assert_eq!(restored.shadow_reserved_balance(), 0);
    }

    #[test]
    fn test_trust_ramp_linear_growth() {
        let refresh_rate = 100u64;
        let state = PeerAccounting::with_refresh_rate(1000, 10000, refresh_rate);

        // Initial state
        assert_eq!(state.local_credit_limit(), 1000);
        assert_eq!(state.credit_limit_grow_at(), 10_000); // 100 * 100

        // Settlement below checkpoint: no growth
        assert!(!state.notify_settlement_received(5_000));
        assert_eq!(state.local_credit_limit(), 1000);
        assert_eq!(state.total_debt_repaid(), 5_000);

        // Settlement crossing checkpoint: growth
        assert!(state.notify_settlement_received(6_000));
        assert_eq!(state.local_credit_limit(), 1100); // 1000 + 100
        assert_eq!(state.total_debt_repaid(), 11_000);
        // Next checkpoint: linear step
        assert_eq!(state.credit_limit_grow_at(), 20_000); // 10_000 + 100*100
    }

    #[test]
    fn test_trust_ramp_exponential_growth() {
        let refresh_rate = 100u64;
        let state = PeerAccounting::with_refresh_rate(1000, 10000, refresh_rate);

        // Set grow_at to the linear->exponential threshold
        let linear_threshold = refresh_rate * LINEAR_CHECKPOINT_COUNT; // 180_000
        state
            .credit_limit_grow_at
            .store(linear_threshold, Ordering::Relaxed);
        state
            .total_debt_repaid
            .store(linear_threshold, Ordering::Relaxed);

        // Next settlement should trigger exponential growth
        assert!(state.notify_settlement_received(1));
        assert_eq!(state.credit_limit_grow_at(), linear_threshold * 2); // doubled
    }

    #[test]
    fn test_trust_ramp_zero_refresh_rate() {
        let state = PeerAccounting::new(1000, 10000);
        // grow_at is 0 when refresh_rate is 0
        assert_eq!(state.credit_limit_grow_at(), 0);
        // Settlement should not trigger growth
        assert!(!state.notify_settlement_received(1000));
        assert_eq!(state.local_credit_limit(), 1000);
    }
}
