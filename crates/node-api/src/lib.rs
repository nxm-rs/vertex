//! Node API traits for the Vertex Swarm node
//!
//! This crate defines traits for node components, configuration, and interfaces.

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::String, vec::Vec};

pub use async_trait::async_trait;
pub use vertex_primitives::{self, ChunkAddress, Error, PeerId, Result};
pub use vertex_swarm_api as swarm_api;
pub use vertex_swarmspec as swarmspec;

/// Node types and components
pub mod node;
pub use node::*;

/// Node builder and configuration
pub mod builder;
pub use builder::*;

/// Node task management
pub mod tasks;
pub use tasks::*;

/// Node events and notifications
pub mod events;
pub use events::*;

/// Node exit handling
pub mod exit;
pub use exit::*;

/// Node startup and shutdown
pub mod lifecycle;
pub use lifecycle::*;
