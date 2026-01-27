use crate::{ForkCondition, Hardfork, SwarmHardforks, hardfork};
use alloc::{boxed::Box, format, string::String};
use core::{
    fmt,
    fmt::{Display, Formatter},
    str::FromStr,
};
use nectar_swarms::{NamedSwarm, Swarm, SwarmKind};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

hardfork!(
    /// The name of a Swarm hardfork.
    SwarmHardfork {
        /// Genesis: The initial Swarm network launch.
        Genesis,
        /// Accord: The Swarm 3.0 network upgrade (vertex-compatible release).
        Accord
    }
);

impl SwarmHardfork {
    /// Mainnet genesis timestamp (June 9, 2021 16:19:47 UTC)
    pub const MAINNET_GENESIS_TIMESTAMP: u64 = 1623255587;

    /// Testnet genesis timestamp (June 9, 2021 16:19:47 UTC)
    pub const TESTNET_GENESIS_TIMESTAMP: u64 = 1623255587;

    /// Dev network Accord timestamp (January 1, 2026 00:00:00 UTC)
    pub const DEV_ACCORD_TIMESTAMP: u64 = 1767225600;

    /// Retrieves the activation timestamp for the specified hardfork on the given swarm.
    pub fn activation_timestamp(&self, swarm: Swarm) -> Option<u64> {
        match swarm.kind() {
            SwarmKind::Named(named) => match named {
                NamedSwarm::Mainnet => self.mainnet_activation_timestamp(),
                NamedSwarm::Testnet => self.testnet_activation_timestamp(),
                NamedSwarm::Dev => self.dev_activation_timestamp(),
                _ => None,
            },
            SwarmKind::Id(_) => None,
        }
    }

    /// Retrieves the activation timestamp for the specified hardfork on the mainnet.
    pub const fn mainnet_activation_timestamp(&self) -> Option<u64> {
        match self {
            Self::Genesis => Some(Self::MAINNET_GENESIS_TIMESTAMP),
            Self::Accord => None, // Not yet scheduled on mainnet
        }
    }

    /// Retrieves the activation timestamp for the specified hardfork on the testnet.
    pub const fn testnet_activation_timestamp(&self) -> Option<u64> {
        match self {
            Self::Genesis => Some(Self::TESTNET_GENESIS_TIMESTAMP),
            Self::Accord => None, // Not yet scheduled on testnet
        }
    }

    /// Retrieves the activation timestamp for the specified hardfork on dev networks.
    pub const fn dev_activation_timestamp(&self) -> Option<u64> {
        match self {
            Self::Genesis => Some(0),
            Self::Accord => Some(Self::DEV_ACCORD_TIMESTAMP),
        }
    }

    /// Mainnet list of hardforks.
    pub const fn mainnet() -> [(Self, ForkCondition); 1] {
        [(
            Self::Genesis,
            ForkCondition::Timestamp(Self::MAINNET_GENESIS_TIMESTAMP),
        )]
    }

    /// Testnet list of hardforks.
    pub const fn testnet() -> [(Self, ForkCondition); 1] {
        [(
            Self::Genesis,
            ForkCondition::Timestamp(Self::TESTNET_GENESIS_TIMESTAMP),
        )]
    }

    /// Dev network list of hardforks.
    pub const fn dev() -> [(Self, ForkCondition); 2] {
        [
            (Self::Genesis, ForkCondition::Timestamp(0)),
            (Self::Accord, ForkCondition::Timestamp(Self::DEV_ACCORD_TIMESTAMP)),
        ]
    }
}

impl<const N: usize> From<[(SwarmHardfork, ForkCondition); N]> for SwarmHardforks {
    fn from(list: [(SwarmHardfork, ForkCondition); N]) -> Self {
        Self::new(
            list.into_iter()
                .map(|(fork, cond)| (Box::new(fork) as Box<dyn Hardfork>, cond))
                .collect(),
        )
    }
}
