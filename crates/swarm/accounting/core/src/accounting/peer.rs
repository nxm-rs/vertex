//! Atomic per-peer balance tracking for lock-free bandwidth recording.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use vertex_swarm_api::{Au, SwarmPeerState};

/// Add `delta` to an atomic balance, saturating at the [`i64`] bounds.
///
/// Plain `fetch_add` wraps on overflow and could flip a balance's sign,
/// inverting owed/owes; a compare-exchange loop saturates instead.
fn saturating_fetch_add(atomic: &AtomicI64, delta: i64) -> i64 {
    let mut current = atomic.load(Ordering::Relaxed);
    loop {
        let next = current.saturating_add(delta);
        match atomic.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return next,
            Err(observed) => current = observed,
        }
    }
}

/// Growth checkpoint step and first checkpoint, in peer-keyed refresh-rate
/// multiples: repayment past each checkpoint raises the serve line one rate.
const GROWTH_STEP_REFRESH_MULTIPLES: u64 = 100;

/// The checkpoint value, in refresh-rate multiples, at which the schedule
/// switches from linear steps to doubling.
const GROWTH_DOUBLING_FLOOR_REFRESH_MULTIPLES: u64 = 1800;

/// The first growth checkpoint for a peer at the given allowance rate.
fn first_checkpoint(rate: Au) -> Au {
    rate.checked_scale(GROWTH_STEP_REFRESH_MULTIPLES)
        .unwrap_or(Au::new(i64::MAX))
}

/// The checkpoint after `checkpoint`: linear `rate * 100` steps while below
/// `rate * 1800`, then doubling. Saturates at the `i64` bound, where growth
/// effectively stops.
fn next_checkpoint(checkpoint: Au, rate: Au) -> Au {
    let doubling_floor = rate
        .checked_scale(GROWTH_DOUBLING_FLOOR_REFRESH_MULTIPLES)
        .unwrap_or(Au::new(i64::MAX));
    if checkpoint < doubling_floor {
        checkpoint.saturating_add(first_checkpoint(rate))
    } else {
        checkpoint.checked_scale(2).unwrap_or(Au::new(i64::MAX))
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
    ghost_balance: AtomicU64,
    payment_threshold: AtomicI64,
    settle_line: AtomicI64,
    disconnect_threshold: Au,
    refresh_allowance: AtomicI64,
    settlement_received: AtomicU64,
    /// The cumulative-repayment value the next serve-line raise triggers past.
    growth_checkpoint: AtomicI64,
}

impl PeerState {
    /// Create peer state with the given thresholds in AU.
    ///
    /// The serve line (`payment_threshold`) is the debt we let the peer owe us
    /// before refusing service; the settle line is the debt we let ourselves owe
    /// the peer before settling, seeded from the local payment threshold and
    /// tightened by the peer's clamped announcement. The two are distinct fields
    /// moving in opposite directions. The refresh allowance is the per-second
    /// settlement rate extended to the peer, healed with the serve line once
    /// the handshake node type is known.
    pub fn new(
        payment_threshold: Au,
        settle_line: Au,
        disconnect_threshold: Au,
        refresh_allowance: Au,
    ) -> Self {
        Self {
            balance: AtomicI64::new(0),
            reserved_balance: AtomicU64::new(0),
            shadow_reserved_balance: AtomicU64::new(0),
            ghost_balance: AtomicU64::new(0),
            payment_threshold: AtomicI64::new(payment_threshold.get()),
            settle_line: AtomicI64::new(settle_line.get()),
            disconnect_threshold,
            refresh_allowance: AtomicI64::new(refresh_allowance.get()),
            settlement_received: AtomicU64::new(0),
            growth_checkpoint: AtomicI64::new(first_checkpoint(refresh_allowance).get()),
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

    /// Get the ghost balance in AU: the accrued prices of provides whose
    /// delivery the peer refused. Never committed and never settled; it only
    /// consumes serve headroom in the provide projection.
    pub fn ghost_balance(&self) -> Au {
        Au::from_amount(self.ghost_balance.load(Ordering::Relaxed))
    }

    /// Add to the ghost balance.
    pub fn add_ghost(&self, amount: Au) {
        self.ghost_balance
            .fetch_add(amount.as_amount(), Ordering::Relaxed);
    }

    /// Get the payment threshold in AU.
    pub fn payment_threshold(&self) -> Au {
        Au::new(self.payment_threshold.load(Ordering::Relaxed))
    }

    /// Update the serve line; the disconnect line is fixed at creation.
    pub fn set_payment_threshold(&self, line: Au) {
        self.payment_threshold.store(line.get(), Ordering::Relaxed);
    }

    /// Get the settle line in AU: the debt we let ourselves owe the peer before
    /// settling toward it.
    pub fn settle_line(&self) -> Au {
        Au::new(self.settle_line.load(Ordering::Relaxed))
    }

    /// Update the settle line from the peer's clamped announcement.
    pub fn set_settle_line(&self, line: Au) {
        self.settle_line.store(line.get(), Ordering::Relaxed);
    }

    /// Get the disconnect threshold in AU.
    pub fn disconnect_threshold(&self) -> Au {
        self.disconnect_threshold
    }

    /// Get the per-second settlement allowance rate in AU.
    pub fn refresh_allowance(&self) -> Au {
        Au::new(self.refresh_allowance.load(Ordering::Relaxed))
    }

    /// Update the allowance rate; healed with the serve line at connect.
    pub fn set_refresh_allowance(&self, rate: Au) {
        self.refresh_allowance.store(rate.get(), Ordering::Relaxed);
    }

    /// Accumulate an accepted inbound settlement and return the new cumulative
    /// total, saturating so a lifetime of repayment can never wrap the signal
    /// threshold growth reads.
    pub fn add_settlement_received(&self, amount: Au) -> Au {
        let delta = amount.as_amount();
        let mut current = self.settlement_received.load(Ordering::Relaxed);
        loop {
            let next = current.saturating_add(delta);
            match self.settlement_received.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Au::from_amount(next),
                Err(observed) => current = observed,
            }
        }
    }

    /// Get the cumulative accepted inbound settlement in AU.
    pub fn settlement_received(&self) -> Au {
        Au::from_amount(self.settlement_received.load(Ordering::Relaxed))
    }

    /// Get the growth checkpoint in AU: the cumulative repayment the next
    /// serve-line raise triggers strictly past.
    pub fn growth_checkpoint(&self) -> Au {
        Au::new(self.growth_checkpoint.load(Ordering::Relaxed))
    }

    /// Reset the repayment accumulator and growth checkpoint for a new
    /// connection at the peer-keyed allowance rate: growth is earned per
    /// connection, starting one step above zero repayment.
    pub fn reset_settlement_growth(&self, rate: Au) {
        self.settlement_received.store(0, Ordering::Relaxed);
        self.growth_checkpoint
            .store(first_checkpoint(rate).get(), Ordering::Relaxed);
    }

    /// Raise the serve line one `rate` step when `total` strictly exceeds the
    /// growth checkpoint, advancing the checkpoint one schedule step. At most
    /// one raise per call, however far `total` overshoots; the CAS claims the
    /// crossing so racing writers never double-raise. Returns the raised line.
    pub fn grow_serve_line(&self, total: Au, rate: Au) -> Option<Au> {
        let checkpoint = self.growth_checkpoint.load(Ordering::Relaxed);
        if total.get() <= checkpoint {
            return None;
        }
        let next = next_checkpoint(Au::new(checkpoint), rate);
        if self
            .growth_checkpoint
            .compare_exchange(checkpoint, next.get(), Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return None;
        }
        Some(Au::new(saturating_fetch_add(
            &self.payment_threshold,
            rate.get(),
        )))
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
        let state = PeerState::new(au(1000), au(1000), au(10000), au(100));

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
        let state = PeerState::new(au(1000), au(1000), au(10000), au(100));
        state.add_balance(Au::new(i64::MAX));
        state.add_balance(au(1000));
        assert_eq!(state.balance(), Au::new(i64::MAX));

        // The negative bound saturates too.
        let state = PeerState::new(au(1000), au(1000), au(10000), au(100));
        state.add_balance(Au::new(i64::MIN));
        state.add_balance(au(-1000));
        assert_eq!(state.balance(), Au::new(i64::MIN));
    }

    #[test]
    fn test_reserved_operations() {
        let state = PeerState::new(au(1000), au(1000), au(10000), au(100));

        assert_eq!(state.reserved_balance(), Au::ZERO);

        state.add_reserved(au(100));
        assert_eq!(state.reserved_balance(), au(100));

        state.sub_reserved(au(50));
        assert_eq!(state.reserved_balance(), au(50));
    }

    #[test]
    fn test_sub_reserved_saturates_at_zero() {
        let state = PeerState::new(au(1000), au(1000), au(10000), au(100));

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
        let state = PeerState::new(au(1000), au(1000), au(10000), au(100));

        assert_eq!(state.payment_threshold(), au(1000));
        assert_eq!(state.disconnect_threshold(), au(10000));
    }

    #[test]
    fn set_payment_threshold_updates_the_serve_line() {
        let state = PeerState::new(au(1000), au(1000), au(10000), au(100));
        assert_eq!(state.payment_threshold(), au(1000));

        state.set_payment_threshold(au(200));
        assert_eq!(state.payment_threshold(), au(200));
        // The disconnect line is fixed at creation and never moves with it.
        assert_eq!(state.disconnect_threshold(), au(10000));
    }

    // Growth schedule at rate 10: checkpoints 1_000, 2_000, ... 18_000 in the
    // linear region (doubling floor 18_000), then 36_000, 72_000.

    #[test]
    fn growth_trigger_is_strictly_greater() {
        let state = PeerState::new(au(1000), au(1000), au(10000), au(10));
        assert_eq!(state.growth_checkpoint(), au(1_000));

        // Exactly at the checkpoint: no raise, checkpoint unmoved.
        assert_eq!(state.grow_serve_line(au(1_000), au(10)), None);
        assert_eq!(state.growth_checkpoint(), au(1_000));
        assert_eq!(state.payment_threshold(), au(1_000));

        // One past it: the serve line rises exactly one rate step.
        assert_eq!(state.grow_serve_line(au(1_001), au(10)), Some(au(1_010)));
        assert_eq!(state.payment_threshold(), au(1_010));
        assert_eq!(state.growth_checkpoint(), au(2_000));
    }

    #[test]
    fn growth_advances_one_step_per_event_however_far_the_total_overshoots() {
        let state = PeerState::new(au(1000), au(1000), au(10000), au(10));

        // A total past several checkpoints still raises once per event.
        assert_eq!(state.grow_serve_line(au(5_500), au(10)), Some(au(1_010)));
        assert_eq!(state.growth_checkpoint(), au(2_000));
        assert_eq!(state.grow_serve_line(au(5_500), au(10)), Some(au(1_020)));
        assert_eq!(state.growth_checkpoint(), au(3_000));
    }

    #[test]
    fn growth_schedule_switches_from_linear_steps_to_doubling() {
        let state = PeerState::new(au(1000), au(1000), au(10000), au(10));

        // Walk the linear region: 17 raises take the checkpoint to 18_000.
        for _ in 0..17 {
            let total = state.growth_checkpoint() + au(1);
            assert!(state.grow_serve_line(total, au(10)).is_some());
        }
        assert_eq!(state.growth_checkpoint(), au(18_000));

        // At the doubling floor the next steps double instead of adding.
        assert!(state.grow_serve_line(au(18_001), au(10)).is_some());
        assert_eq!(state.growth_checkpoint(), au(36_000));
        assert!(state.grow_serve_line(au(36_001), au(10)).is_some());
        assert_eq!(state.growth_checkpoint(), au(72_000));
    }

    #[test]
    fn reset_settlement_growth_restarts_the_schedule_at_the_given_rate() {
        let state = PeerState::new(au(1000), au(1000), au(10000), au(10));
        state.add_settlement_received(au(5_000));
        assert!(state.grow_serve_line(au(5_000), au(10)).is_some());

        // A reconnect at a different peer-keyed rate restarts both the
        // accumulator and the checkpoint from that rate's first step.
        state.reset_settlement_growth(au(20));
        assert_eq!(state.settlement_received(), Au::ZERO);
        assert_eq!(state.growth_checkpoint(), au(2_000));
    }
}
