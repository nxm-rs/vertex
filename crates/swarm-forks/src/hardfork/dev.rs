use alloc::vec;

use once_cell as _;
#[cfg(not(feature = "std"))]
use once_cell::sync::Lazy as LazyLock;
#[cfg(feature = "std")]
use std::sync::LazyLock;

use crate::{ForkCondition, Hardfork, SwarmHardfork, SwarmHardforks};

/// Dev hardforks - all active at timestamp 0 for development purposes
pub static DEV_HARDFORKS: LazyLock<SwarmHardforks> = LazyLock::new(|| {
    SwarmHardforks::new(vec![(
        SwarmHardfork::Frontier.boxed(),
        ForkCondition::Timestamp(0),
    )])
});
