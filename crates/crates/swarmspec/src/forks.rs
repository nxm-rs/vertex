//! Swarm hardforks and fork conditions

use core::fmt;
#[cfg(feature = "serde")]
use serde::{Serialize, Deserialize};

/// The condition at which a fork is activated.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum ForkCondition {
    /// The fork is activated after a certain timestamp.
    Timestamp(u64),
    /// The fork is never activated
    #[default]
    Never,
}

impl ForkCondition {
    /// Checks whether the fork condition is satisfied at the given timestamp.
    pub const fn active_at_timestamp(&self, timestamp: u64) -> bool {
        matches!(self, Self::Timestamp(time) if timestamp >= *time)
    }

    /// Returns the timestamp of the fork condition, if it is timestamp based.
    pub const fn as_timestamp(&self) -> Option<u64> {
        match self {
            Self::Timestamp(timestamp) => Some(*timestamp),
            _ => None,
        }
    }
}

/// The name of a Swarm hardfork.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum SwarmHardfork {
    /// Frontier: The initial network launch.
    Frontier,
    /// Future planned hardfork (example)
    /// Sphinx,
}

impl SwarmHardfork {
    /// Mainnet timestamp for the Frontier hardfork activation (June 9, 2021 16:19:47 UTC)
    pub const MAINNET_GENESIS_TIMESTAMP: u64 = 1623255587;

    /// Testnet timestamp for the Frontier hardfork activation (June 9, 2021 16:19:47 UTC)
    pub const TESTNET_GENESIS_TIMESTAMP: u64 = 1623255587;

    /// Returns variant as `str`.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Frontier => "Frontier",
            // Self::Sphinx => "Sphinx",
        }
    }

    /// Retrieves the activation timestamp for the specified hardfork on the mainnet.
    pub const fn mainnet_activation_timestamp(&self) -> Option<u64> {
        match self {
            Self::Frontier => Some(Self::MAINNET_GENESIS_TIMESTAMP),
            // Self::Sphinx => Some(future_timestamp),
        }
    }

    /// Retrieves the activation timestamp for the specified hardfork on the testnet.
    pub const fn testnet_activation_timestamp(&self) -> Option<u64> {
        match self {
            Self::Frontier => Some(Self::TESTNET_GENESIS_TIMESTAMP),
            // Self::Sphinx => Some(future_timestamp),
        }
    }
}

impl fmt::Display for SwarmHardfork {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A container for Swarm network hardforks
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct SwarmHardforks {
    /// Map of hardforks to their activation conditions
    forks: alloc::collections::BTreeMap<SwarmHardfork, ForkCondition>,
}

impl SwarmHardforks {
    /// Create a new empty hardforks container
    pub fn new() -> Self {
        Self { forks: alloc::collections::BTreeMap::new() }
    }

    /// Insert a fork with an activation condition
    pub fn insert(&mut self, fork: SwarmHardfork, condition: ForkCondition) -> Option<ForkCondition> {
        self.forks.insert(fork, condition)
    }

    /// Get the activation condition for a fork
    pub fn get(&self, fork: SwarmHardfork) -> Option<ForkCondition> {
        self.forks.get(&fork).copied()
    }

    /// Check if a fork is active at a given timestamp
    pub fn is_active_at_timestamp(&self, fork: SwarmHardfork, timestamp: u64) -> bool {
        self.get(fork).map_or(false, |cond| cond.active_at_timestamp(timestamp))
    }

    /// Return an iterator over all forks and their conditions
    pub fn iter(&self) -> impl Iterator<Item = (&SwarmHardfork, &ForkCondition)> {
        self.forks.iter()
    }
}
