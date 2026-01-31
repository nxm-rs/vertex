//! Accounting actions for prepare/apply pattern.
//!
//! Actions represent pending balance changes that must be either applied
//! (committed) or cleaned up (released) when dropped.

use std::sync::Arc;

use super::PeerState;

/// Trait for accounting actions.
pub trait AccountingAction: Send {
    /// Apply the action, committing the balance change.
    fn apply(self);

    /// Clean up without applying (releases reservations).
    fn cleanup(&self);
}

/// Credit action for receiving service (balance decreases).
///
/// When we request a chunk from a peer, we create a credit action which:
/// 1. Reserves the price from our balance
/// 2. On apply(): commits the balance decrease
/// 3. On drop without apply(): releases the reservation
pub struct CreditAction {
    state: Arc<PeerState>,
    price: u64,
    applied: bool,
}

impl CreditAction {
    /// Create a new credit action.
    pub fn new(state: Arc<PeerState>, price: u64) -> Self {
        Self {
            state,
            price,
            applied: false,
        }
    }

    /// Apply the credit, committing the balance decrease.
    pub fn apply(mut self) {
        self.state.add_balance(-(self.price as i64));
        self.state.sub_reserved(self.price);
        self.applied = true;
    }
}

impl Drop for CreditAction {
    fn drop(&mut self) {
        if !self.applied {
            self.state.sub_reserved(self.price);
        }
    }
}

impl AccountingAction for CreditAction {
    fn apply(self) {
        CreditAction::apply(self);
    }

    fn cleanup(&self) {}
}

/// Debit action for providing service (balance increases).
///
/// When we send a chunk to a peer, we create a debit action which:
/// 1. Reserves the expected incoming balance (shadow)
/// 2. On apply(): commits the balance increase
/// 3. On drop without apply(): releases the shadow reservation
pub struct DebitAction {
    state: Arc<PeerState>,
    price: u64,
    applied: bool,
}

impl DebitAction {
    /// Create a new debit action.
    pub fn new(state: Arc<PeerState>, price: u64) -> Self {
        Self {
            state,
            price,
            applied: false,
        }
    }

    /// Apply the debit, committing the balance increase.
    pub fn apply(mut self) {
        self.state.add_balance(self.price as i64);
        self.state.sub_shadow_reserved(self.price);
        self.applied = true;
    }
}

impl Drop for DebitAction {
    fn drop(&mut self) {
        if !self.applied {
            self.state.sub_shadow_reserved(self.price);
        }
    }
}

impl AccountingAction for DebitAction {
    fn apply(self) {
        DebitAction::apply(self);
    }

    fn cleanup(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_primitives::OverlayAddress;

    fn test_peer() -> OverlayAddress {
        OverlayAddress::from([1u8; 32])
    }

    #[test]
    fn test_credit_action_apply() {
        let state = Arc::new(PeerState::new(test_peer(), 1000, 10000));
        state.add_reserved(100);

        let action = CreditAction::new(Arc::clone(&state), 100);
        action.apply();

        assert_eq!(state.balance(), -100);
        assert_eq!(state.reserved_balance(), 0);
    }

    #[test]
    fn test_credit_action_drop() {
        let state = Arc::new(PeerState::new(test_peer(), 1000, 10000));
        state.add_reserved(100);

        {
            let _action = CreditAction::new(Arc::clone(&state), 100);
        }

        assert_eq!(state.balance(), 0);
        assert_eq!(state.reserved_balance(), 0);
    }

    #[test]
    fn test_debit_action_apply() {
        let state = Arc::new(PeerState::new(test_peer(), 1000, 10000));
        state.add_shadow_reserved(100);

        let action = DebitAction::new(Arc::clone(&state), 100);
        action.apply();

        assert_eq!(state.balance(), 100);
        assert_eq!(state.shadow_reserved_balance(), 0);
    }

    #[test]
    fn test_debit_action_drop() {
        let state = Arc::new(PeerState::new(test_peer(), 1000, 10000));
        state.add_shadow_reserved(100);

        {
            let _action = DebitAction::new(Arc::clone(&state), 100);
        }

        assert_eq!(state.balance(), 0);
        assert_eq!(state.shadow_reserved_balance(), 0);
    }
}
