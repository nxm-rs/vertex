//! Prepare/apply pattern for balance changes.
//!
//! Actions reserve balance, then either apply (commit) or clean up on drop.

use std::sync::Arc;

use super::PeerAccounting;

/// Direction of a balance action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionDirection {
    /// Receiving service from a peer (balance decreases).
    Receive,
    /// Providing service to a peer (balance increases).
    Provide,
}

/// Action for a balance change in either direction.
///
/// Reserves balance on creation; commits on apply(), releases on drop.
pub struct BalanceAction {
    state: Arc<PeerAccounting>,
    price: u64,
    direction: ActionDirection,
    applied: bool,
}

impl BalanceAction {
    /// Create a new balance action.
    pub fn new(state: Arc<PeerAccounting>, price: u64, direction: ActionDirection) -> Self {
        Self {
            state,
            price,
            direction,
            applied: false,
        }
    }

    /// Apply the action, committing the balance change.
    pub fn apply(mut self) {
        match self.direction {
            ActionDirection::Receive => {
                self.state.add_balance(-(self.price as i64));
                self.state.sub_reserved(self.price);
            }
            ActionDirection::Provide => {
                self.state.add_balance(self.price as i64);
                self.state.sub_shadow_reserved(self.price);
            }
        }
        self.applied = true;
    }
}

impl Drop for BalanceAction {
    fn drop(&mut self) {
        if !self.applied {
            match self.direction {
                ActionDirection::Receive => self.state.sub_reserved(self.price),
                ActionDirection::Provide => self.state.sub_shadow_reserved(self.price),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_receive_action_apply() {
        let state = Arc::new(PeerAccounting::new(1000, 10000));
        state.add_reserved(100);

        let action = BalanceAction::new(Arc::clone(&state), 100, ActionDirection::Receive);
        action.apply();

        assert_eq!(state.balance(), -100);
        assert_eq!(state.reserved_balance(), 0);
    }

    #[test]
    fn test_receive_action_drop() {
        let state = Arc::new(PeerAccounting::new(1000, 10000));
        state.add_reserved(100);

        {
            let _action = BalanceAction::new(Arc::clone(&state), 100, ActionDirection::Receive);
        }

        assert_eq!(state.balance(), 0);
        assert_eq!(state.reserved_balance(), 0);
    }

    #[test]
    fn test_provide_action_apply() {
        let state = Arc::new(PeerAccounting::new(1000, 10000));
        state.add_shadow_reserved(100);

        let action = BalanceAction::new(Arc::clone(&state), 100, ActionDirection::Provide);
        action.apply();

        assert_eq!(state.balance(), 100);
        assert_eq!(state.shadow_reserved_balance(), 0);
    }

    #[test]
    fn test_provide_action_drop() {
        let state = Arc::new(PeerAccounting::new(1000, 10000));
        state.add_shadow_reserved(100);

        {
            let _action = BalanceAction::new(Arc::clone(&state), 100, ActionDirection::Provide);
        }

        assert_eq!(state.balance(), 0);
        assert_eq!(state.shadow_reserved_balance(), 0);
    }
}
