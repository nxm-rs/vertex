//! Swarm hardfork definitions and activation logic.
//!
//! Hardforks in Swarm are timestamp-based protocol upgrades. Unlike Ethereum's
//! block-based forks, Swarm nodes activate new behavior when `block.timestamp`
//! exceeds the fork's activation time.
//!
//! # Core Types
//!
//! - [`SwarmHardfork`] - Enum of all Swarm protocol upgrades
//! - [`ForkCondition`] - When a fork activates (timestamp or "never")
//! - [`Hardforks`] - Collection mapping hardforks to their activation times
//!
//! # Usage
//!
//! ```ignore
//! use vertex_swarm_forks::{SwarmHardfork, ForkCondition};
//!
//! let condition = ForkCondition::Timestamp(1699999999);
//! if condition.active_at_timestamp(current_time) {
//!     // Use post-fork protocol
//! }
//! ```

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

// Used for feature forwarding (serde, arbitrary, std)
use alloy_primitives as _;

/// Re-exported EIP-2124 forkid types for network compatibility.
pub use alloy_eip2124::*;

mod display;
mod forkcondition;
mod hardfork;
mod hardforks;

pub use hardfork::{DEV_HARDFORKS, Hardfork, SwarmHardfork};

pub use display::DisplayHardforks;
pub use forkcondition::ForkCondition;
pub use hardforks::*;

#[cfg(any(test, feature = "arbitrary"))]
pub use arbitrary;
