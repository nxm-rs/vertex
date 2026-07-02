//! Typed reservation legs: a single RAII hold with the leg as a typestate.
//!
//! A reservation reserves balance on creation, commits on [`apply`](Reservation::apply),
//! and releases the leg's reserved counter on drop. The leg is a sealed trait so
//! one `Drop` impl dispatches for both legs (a per-leg `Drop` is rejected as
//! `E0367`). The deferred provide commit crosses the api boundary through the
//! object-safe [`CommitOnWrite`](vertex_swarm_api::CommitOnWrite) trait, which
//! only the provide leg implements.

use std::marker::PhantomData;
use std::sync::Arc;

use vertex_swarm_api::Au;

use super::PeerState;

mod sealed {
    pub trait Sealed {}
}

/// A reservation leg: how a committed amount applies and how its reserve releases.
pub trait Leg: sealed::Sealed {
    /// Commit the leg's amount into the balance.
    #[doc(hidden)]
    fn apply(state: &PeerState, price: Au);

    /// Release the leg's reserved counter.
    #[doc(hidden)]
    fn release(state: &PeerState, price: Au);
}

/// Receiving service: we owe the peer. Reserves `reserved_balance`.
pub struct Receive;

/// Providing service: the peer owes us. Reserves `shadow_reserved_balance`.
pub struct Provide;

impl sealed::Sealed for Receive {}
impl sealed::Sealed for Provide {}

impl Leg for Receive {
    fn apply(state: &PeerState, price: Au) {
        state.add_balance(-price);
        state.sub_reserved(price);
    }

    fn release(state: &PeerState, price: Au) {
        state.sub_reserved(price);
    }
}

impl Leg for Provide {
    fn apply(state: &PeerState, price: Au) {
        state.add_balance(price);
        state.sub_shadow_reserved(price);
    }

    fn release(state: &PeerState, price: Au) {
        state.sub_shadow_reserved(price);
    }
}

/// A reserved balance change for one leg, applied on success or released on drop.
///
/// The two-leg relay seam: a forwarder holds both legs and applies them only when
/// the relay succeeds, so a failed relay releases every reservation on drop and
/// never leaks. A leg mismatch is a compile error, not a runtime branch.
pub struct Reservation<L: Leg> {
    state: Arc<PeerState>,
    price: Au,
    applied: bool,
    _leg: PhantomData<L>,
}

impl<L: Leg> Reservation<L> {
    pub(super) fn new(state: Arc<PeerState>, price: Au) -> Self {
        Self {
            state,
            price,
            applied: false,
            _leg: PhantomData,
        }
    }

    /// Commit the reserved balance change.
    pub fn apply(mut self) {
        L::apply(&self.state, self.price);
        self.applied = true;
    }
}

impl Reservation<Provide> {
    /// Release the reservation but accrue its price as ghost debt: the answer
    /// was in hand and the peer refused to take delivery. The ghost is never
    /// committed or settled; it consumes serve headroom in the provide
    /// projection so a repeat refuser starves. Our-fault failures drop
    /// instead, releasing without a trace.
    pub fn forfeit(self) {
        self.state.add_ghost(self.price);
        // Drop releases the shadow reservation.
    }
}

impl<L: Leg> Drop for Reservation<L> {
    fn drop(&mut self) {
        if !self.applied {
            L::release(&self.state, self.price);
        }
    }
}

impl vertex_swarm_api::Commit for Reservation<Receive> {
    fn apply(self) {
        Reservation::apply(self);
    }
}

impl vertex_swarm_api::CommitOnWrite for Reservation<Provide> {
    fn apply_boxed(self: Box<Self>) {
        Reservation::apply(*self);
    }

    fn forfeit_boxed(self: Box<Self>) {
        Reservation::forfeit(*self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn au(value: i64) -> Au {
        Au::new(value)
    }

    #[test]
    fn receive_apply_commits_balance_and_clears_reserve() {
        let state = Arc::new(PeerState::new(au(1000), au(10000)));
        state.add_reserved(au(100));

        Reservation::<Receive>::new(Arc::clone(&state), au(100)).apply();

        assert_eq!(state.balance(), au(-100));
        assert_eq!(state.reserved_balance(), Au::ZERO);
    }

    #[test]
    fn receive_drop_releases_reserve_only() {
        let state = Arc::new(PeerState::new(au(1000), au(10000)));
        state.add_reserved(au(100));

        drop(Reservation::<Receive>::new(Arc::clone(&state), au(100)));

        assert_eq!(state.balance(), Au::ZERO);
        assert_eq!(state.reserved_balance(), Au::ZERO);
    }

    #[test]
    fn provide_apply_commits_balance_and_clears_shadow_reserve() {
        let state = Arc::new(PeerState::new(au(1000), au(10000)));
        state.add_shadow_reserved(au(100));

        Reservation::<Provide>::new(Arc::clone(&state), au(100)).apply();

        assert_eq!(state.balance(), au(100));
        assert_eq!(state.shadow_reserved_balance(), Au::ZERO);
    }

    #[test]
    fn provide_drop_releases_shadow_reserve_only() {
        let state = Arc::new(PeerState::new(au(1000), au(10000)));
        state.add_shadow_reserved(au(100));

        drop(Reservation::<Provide>::new(Arc::clone(&state), au(100)));

        assert_eq!(state.balance(), Au::ZERO);
        assert_eq!(state.shadow_reserved_balance(), Au::ZERO);
        assert_eq!(state.ghost_balance(), Au::ZERO);
    }

    #[test]
    fn provide_forfeit_releases_shadow_reserve_and_accrues_ghost() {
        let state = Arc::new(PeerState::new(au(1000), au(10000)));
        state.add_shadow_reserved(au(100));

        Reservation::<Provide>::new(Arc::clone(&state), au(100)).forfeit();

        assert_eq!(state.balance(), Au::ZERO, "never committed");
        assert_eq!(state.shadow_reserved_balance(), Au::ZERO, "released");
        assert_eq!(state.ghost_balance(), au(100), "the refusal leaves a trace");
    }
}
