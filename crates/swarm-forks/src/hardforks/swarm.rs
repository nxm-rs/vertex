use crate::{ForkCondition, SwarmHardfork};

/// Helper methods for Swarm forks.
///
/// This trait provides convenience methods for checking the activation status
/// of various hardforks.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmHardforksTrait: Clone {
    /// Retrieves [`ForkCondition`] by an [`SwarmHardfork`]. If `fork` is not present, returns
    /// [`ForkCondition::Never`].
    fn swarm_fork_activation(&self, fork: SwarmHardfork) -> ForkCondition;

    /// Convenience method to check if an [`SwarmHardfork`] is active at a given timestamp.
    fn is_swarm_fork_active_at_timestamp(&self, fork: SwarmHardfork, timestamp: u64) -> bool {
        self.swarm_fork_activation(fork)
            .active_at_timestamp(timestamp)
    }

    /// Convenience method to check if an [`SwarmHardfork`] is active at a given block number.
    fn is_swarm_fork_active_at_block(&self, fork: SwarmHardfork, block_number: u64) -> bool {
        self.swarm_fork_activation(fork)
            .active_at_block(block_number)
    }

    /// Convenience method to check if [`SwarmHardfork::Frontier`] is active at a given timestamp.
    fn is_frontier_active_at_timestamp(&self, timestamp: u64) -> bool {
        self.is_swarm_fork_active_at_timestamp(SwarmHardfork::Frontier, timestamp)
    }
}
