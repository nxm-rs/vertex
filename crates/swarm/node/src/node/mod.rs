//! Node types for Swarm network participation.
//!
//! - [`BootNode`] - Topology only (bootnode servers); native-only.
//! - [`ClientNode`] - Topology + client protocols (chunk read/write).
//! - [`StorerNode`] - Client + storage protocols (chunk storage and staking);
//!   native-only.
//!
//! The browser client target builds only the [`ClientNode`] path. Bootnode and
//! storer are out of scope for `wasm32-unknown-unknown` (they need listeners,
//! NAT traversal, and native storage), so their modules are native-only.

mod base;
#[cfg(not(target_arch = "wasm32"))]
#[allow(unreachable_pub)]
mod bootnode;
mod builder;
#[allow(unreachable_pub)]
mod client;
mod error;
pub(crate) mod stats;
#[cfg(not(target_arch = "wasm32"))]
mod storer;
pub(crate) mod task;

pub use base::BaseNode;
#[cfg(not(target_arch = "wasm32"))]
pub use bootnode::{BootNode, BootNodeBuilder};
pub use builder::BuiltInfrastructure;
pub use client::{ClientNode, ClientNodeBuilder};
pub use error::NodeBuildError;
#[cfg(not(target_arch = "wasm32"))]
pub use storer::{StorerNode, StorerNodeBuilder};
