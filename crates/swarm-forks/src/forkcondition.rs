use crate::Head;
use alloy_primitives::BlockNumber;

/// The condition at which a fork is activated.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ForkCondition {
    /// The fork is activated after a certain block.
    Block(BlockNumber),
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

    /// Checks whether the fork condition is satisfied at the given block.
    ///
    /// This will return true if the block number is equal or greater than the activation block of:
    /// - [`ForkCondition::Block`]
    ///
    /// For timestamp conditions, this will always return false.
    pub const fn active_at_block(&self, current_block: BlockNumber) -> bool {
        matches!(self, Self::Block(block) if current_block >= *block)
    }

    /// Checks if the given block is the first block that satisfies the fork condition.
    ///
    /// This will return false for any condition that is not block based.
    pub const fn transitions_at_block(&self, current_block: BlockNumber) -> bool {
        matches!(self, Self::Block(block) if current_block == *block)
    }

    /// Checks whether the fork condition is satisfied at the given timestamp.
    ///
    /// This will return false for any condition that is not timestamp-based.
    pub const fn active_at_timestamp(&self, timestamp: u64) -> bool {
        matches!(self, Self::Timestamp(time) if timestamp >= *time)
    }

    /// Checks if the given block is the first block that satisfies the fork condition.
    ///
    /// This will return false for any condition that is not timestamp based.
    pub const fn transitions_at_timestamp(&self, timestamp: u64, parent_timestamp: u64) -> bool {
        matches!(self, Self::Timestamp(time) if timestamp >= *time && parent_timestamp < *time)
    }

    /// Checks whether the fork condition is satisfied at the given timestamp or number.
    pub const fn active_at_timestamp_or_number(&self, timestamp: u64, block_number: u64) -> bool {
        self.active_at_timestamp(timestamp) || self.active_at_block(block_number)
    }

    /// Checks whether the fork condition is satisfied at the given head block.
    ///
    /// This will return true if:
    ///
    /// - The condition is satisfied by the block number; or
    /// - The condition is satisfied by the timestamp
    pub fn active_at_head(&self, head: &Head) -> bool {
        self.active_at_timestamp_or_number(head.timestamp, head.number)
    }

    /// Returns the timestamp of the fork condition, if it is timestamp based.
    pub const fn as_timestamp(&self) -> Option<u64> {
        match self {
            Self::Timestamp(timestamp) => Some(*timestamp),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;

    #[test]
    fn test_active_at_timestamp() {
        // Test if the condition activates at the correct timestamp
        let fork_condition = ForkCondition::Timestamp(1631112000);
        assert!(
            fork_condition.active_at_timestamp(1631112000),
            "The condition should be active at timestamp 1631112000"
        );

        // Test if the condition does not activate at an earlier timestamp
        assert!(
            !fork_condition.active_at_timestamp(1631111999),
            "The condition should not be active at an earlier timestamp"
        );
    }

    #[test]
    fn test_transitions_at_timestamp() {
        // Test if the condition transitions at the correct timestamp
        let fork_condition = ForkCondition::Timestamp(1631112000);
        assert!(
            fork_condition.transitions_at_timestamp(1631112000, 1631111999),
            "The condition should transition at timestamp 1631112000"
        );

        // Test if the condition does not transition if the parent timestamp is already the same
        assert!(
            !fork_condition.transitions_at_timestamp(1631112000, 1631112000),
            "The condition should not transition if the parent timestamp is already 1631112000"
        );
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

        // Test if the condition activates based on timestamp
        let fork_condition = ForkCondition::Timestamp(1631112000);
        assert!(
            fork_condition.active_at_head(&head),
            "The condition should be active at the given head timestamp"
        );

        let fork_condition = ForkCondition::Timestamp(1631112001);
        assert!(
            !fork_condition.active_at_head(&head),
            "The condition should not be active at the given head timestamp"
        );
    }
}
