use crate::{ForkCondition, SwarmHardfork};

/// Helper methods for Swarm forks.
///
/// This trait provides convenience methods for checking the activation status
/// of various hardforks. Swarm uses timestamp-based activation exclusively.
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

    /// Convenience method to check if [`SwarmHardfork::Accord`] is active at a given timestamp.
    fn is_accord_active_at_timestamp(&self, timestamp: u64) -> bool {
        self.is_swarm_fork_active_at_timestamp(SwarmHardfork::Accord, timestamp)
    }
}
