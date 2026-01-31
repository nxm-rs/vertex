//! Prepare/apply pattern for balance changes.
//!
//! Actions reserve balance, then either apply (commit) or clean up on drop.

use std::sync::Arc;

use super::PeerState;

/// Trait for accounting actions.
pub trait AccountingAction: Send {
    /// Apply the action, committing the balance change.
    fn apply(self);

    /// Clean up without applying (releases reservations).
    fn cleanup(&self);
}

/// Action for receiving service from a peer (balance decreases).
///
/// Reserves balance on creation; commits on apply(), releases on drop.
pub struct ReceiveAction {
    state: Arc<PeerState>,
    price: u64,
    applied: bool,
}

impl ReceiveAction {
    /// Create a new receive action.
    pub fn new(state: Arc<PeerState>, price: u64) -> Self {
        Self {
            state,
            price,
            applied: false,
        }
    }

    /// Apply the action, committing the balance decrease.
    pub fn apply(mut self) {
        self.state.add_balance(-(self.price as i64));
        self.state.sub_reserved(self.price);
        self.applied = true;
    }
}

impl Drop for ReceiveAction {
    fn drop(&mut self) {
        if !self.applied {
            self.state.sub_reserved(self.price);
        }
    }
}

impl AccountingAction for ReceiveAction {
    fn apply(self) {
        ReceiveAction::apply(self);
    }

    fn cleanup(&self) {}
}

/// Action for providing service to a peer (balance increases).
///
/// Reserves shadow balance on creation; commits on apply(), releases on drop.
pub struct ProvideAction {
    state: Arc<PeerState>,
    price: u64,
    applied: bool,
}

impl ProvideAction {
    /// Create a new provide action.
    pub fn new(state: Arc<PeerState>, price: u64) -> Self {
        Self {
            state,
            price,
            applied: false,
        }
    }

    /// Apply the action, committing the balance increase.
    pub fn apply(mut self) {
        self.state.add_balance(self.price as i64);
        self.state.sub_shadow_reserved(self.price);
        self.applied = true;
    }
}

impl Drop for ProvideAction {
    fn drop(&mut self) {
        if !self.applied {
            self.state.sub_shadow_reserved(self.price);
        }
    }
}

impl AccountingAction for ProvideAction {
    fn apply(self) {
        ProvideAction::apply(self);
    }

    fn cleanup(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_receive_action_apply() {
        let state = Arc::new(PeerState::new(1000, 10000));
        state.add_reserved(100);

        let action = ReceiveAction::new(Arc::clone(&state), 100);
        action.apply();

        assert_eq!(state.balance(), -100);
        assert_eq!(state.reserved_balance(), 0);
    }

    #[test]
    fn test_receive_action_drop() {
        let state = Arc::new(PeerState::new(1000, 10000));
        state.add_reserved(100);

        {
            let _action = ReceiveAction::new(Arc::clone(&state), 100);
        }

        assert_eq!(state.balance(), 0);
        assert_eq!(state.reserved_balance(), 0);
    }

    #[test]
    fn test_provide_action_apply() {
        let state = Arc::new(PeerState::new(1000, 10000));
        state.add_shadow_reserved(100);

        let action = ProvideAction::new(Arc::clone(&state), 100);
        action.apply();

        assert_eq!(state.balance(), 100);
        assert_eq!(state.shadow_reserved_balance(), 0);
    }

    #[test]
    fn test_provide_action_drop() {
        let state = Arc::new(PeerState::new(1000, 10000));
        state.add_shadow_reserved(100);

        {
            let _action = ProvideAction::new(Arc::clone(&state), 100);
        }

        assert_eq!(state.balance(), 0);
        assert_eq!(state.shadow_reserved_balance(), 0);
    }
}
