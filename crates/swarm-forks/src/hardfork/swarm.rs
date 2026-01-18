use crate::{hardfork, ForkCondition, Hardfork, SwarmHardforks};
use alloc::{boxed::Box, format, string::String};
use core::{
    fmt,
    fmt::{Display, Formatter},
    str::FromStr,
};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use nectar_swarms::{NamedSwarm, Swarm, SwarmKind};

hardfork!(
    /// The name of a Swarm hardfork.
    SwarmHardfork {
        /// Accord: The Swarm 3.0 network (initial vertex-compatible release).
        Accord
    }
);

impl SwarmHardfork {
    /// Mainnet timestamp for the hardfork activation (June 9, 2021 16:19:47 UTC)
    pub const MAINNET_GENESIS_TIMESTAMP: u64 = 1623255587;

    /// Testnet timestamp for the hardfork activation (June 9, 2021 16:19:47 UTC)
    pub const TESTNET_GENESIS_TIMESTAMP: u64 = 1623255587;

    /// Retrieves the activation timestamp for the specified hardfork on the given swarm.
    pub fn activation_timestamp(&self, swarm: Swarm) -> Option<u64> {
        match swarm.kind() {
            SwarmKind::Named(named) => match named {
                NamedSwarm::Mainnet => self.mainnet_activation_timestamp(),
                NamedSwarm::Testnet => self.testnet_activation_timestamp(),
                NamedSwarm::Dev => Some(0),
                _ => None,
            },
            SwarmKind::Id(_) => None,
        }
    }

    /// Retrieves the activation timestamp for the specified hardfork on the mainnet.
    pub const fn mainnet_activation_timestamp(&self) -> Option<u64> {
        match self {
            Self::Accord => Some(Self::MAINNET_GENESIS_TIMESTAMP),
            // Add additional hardforks here as they are defined
        }
    }

    /// Retrieves the activation timestamp for the specified hardfork on the testnet.
    pub const fn testnet_activation_timestamp(&self) -> Option<u64> {
        match self {
            Self::Accord => Some(Self::TESTNET_GENESIS_TIMESTAMP),
            // Add additional hardforks here as they are defined
        }
    }

    /// Mainnet list of hardforks.
    pub const fn mainnet() -> [(Self, ForkCondition); 1] {
        [(
            Self::Accord,
            ForkCondition::Timestamp(Self::MAINNET_GENESIS_TIMESTAMP),
        )]
    }

    /// Testnet list of hardforks.
    pub const fn testnet() -> [(Self, ForkCondition); 1] {
        [(
            Self::Accord,
            ForkCondition::Timestamp(Self::TESTNET_GENESIS_TIMESTAMP),
        )]
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
