use crate::Head;

/// The condition at which a fork is activated.
///
/// Swarm uses timestamp-based fork activation exclusively, as the network
/// does not have block-based consensus like Ethereum.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ForkCondition {
    /// The fork is activated after a specific timestamp.
    Timestamp(u64),
    /// The fork is never activated
    #[default]
    Never,
}

impl ForkCondition {
    /// Returns true if the fork condition is timestamp based.
    pub const fn is_timestamp(&self) -> bool {
        matches!(self, Self::Timestamp(_))
    }

    /// Checks whether the fork condition is satisfied at the given timestamp.
    pub const fn active_at_timestamp(&self, timestamp: u64) -> bool {
        matches!(self, Self::Timestamp(time) if timestamp >= *time)
    }

    /// Checks if the given timestamp is the first that satisfies the fork condition.
    ///
    /// Returns true if this timestamp activates the fork and the parent timestamp
    /// was before the activation.
    pub const fn transitions_at_timestamp(&self, timestamp: u64, parent_timestamp: u64) -> bool {
        matches!(self, Self::Timestamp(time) if timestamp >= *time && parent_timestamp < *time)
    }

    /// Checks whether the fork condition is satisfied at the given head.
    pub fn active_at_head(&self, head: &Head) -> bool {
        self.active_at_timestamp(head.timestamp)
    }

    /// Returns the timestamp of the fork condition, if it is timestamp based.
    pub const fn as_timestamp(&self) -> Option<u64> {
        match self {
            Self::Timestamp(timestamp) => Some(*timestamp),
            Self::Never => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;

    #[test]
    fn test_active_at_timestamp() {
        let fork_condition = ForkCondition::Timestamp(1631112000);

        // Should be active at exact timestamp
        assert!(fork_condition.active_at_timestamp(1631112000));

        // Should be active after timestamp
        assert!(fork_condition.active_at_timestamp(1631112001));

        // Should not be active before timestamp
        assert!(!fork_condition.active_at_timestamp(1631111999));

        // Never condition should never be active
        assert!(!ForkCondition::Never.active_at_timestamp(u64::MAX));
    }

    #[test]
    fn test_transitions_at_timestamp() {
        let fork_condition = ForkCondition::Timestamp(1631112000);

        // Should transition when crossing the activation timestamp
        assert!(fork_condition.transitions_at_timestamp(1631112000, 1631111999));

        // Should not transition if parent is already at or after activation
        assert!(!fork_condition.transitions_at_timestamp(1631112000, 1631112000));
        assert!(!fork_condition.transitions_at_timestamp(1631112001, 1631112000));
    }

    #[test]
    fn test_active_at_head() {
        let head = Head {
            hash: Default::default(),
            number: 10,
            timestamp: 1631112000,
            total_difficulty: U256::from(1000),
            difficulty: U256::from(100),
        };

        // Should be active at exact timestamp
        let fork_condition = ForkCondition::Timestamp(1631112000);
        assert!(fork_condition.active_at_head(&head));

        // Should not be active for future timestamp
        let fork_condition = ForkCondition::Timestamp(1631112001);
        assert!(!fork_condition.active_at_head(&head));
    }

    #[test]
    fn test_as_timestamp() {
        assert_eq!(ForkCondition::Timestamp(123).as_timestamp(), Some(123));
        assert_eq!(ForkCondition::Never.as_timestamp(), None);
    }

    #[test]
    fn test_is_timestamp() {
        assert!(ForkCondition::Timestamp(123).is_timestamp());
        assert!(!ForkCondition::Never.is_timestamp());
    }
}
