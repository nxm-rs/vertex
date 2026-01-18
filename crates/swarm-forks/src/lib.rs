//! Swarm fork types for the network.
//!
//! This crate contains network fork types and helper functions for managing
//! hardforks in a timestamp-based activation model.
//!
//! ## Feature Flags
//!
//! - `arbitrary`: Adds `arbitrary` support for primitive types.
//! - `serde`: Adds serialization/deserialization capabilities.
//! - `std`: Uses standard library (default feature).
//! - `rustc-hash`: Uses rustc's hash implementation (default feature).

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

pub use hardfork::{Hardfork, SwarmHardfork, DEV_HARDFORKS};

pub use display::DisplayHardforks;
pub use forkcondition::ForkCondition;
pub use hardforks::*;

#[cfg(any(test, feature = "arbitrary"))]
pub use arbitrary;
