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
mod core;
mod error;
mod launch;
// NAT traversal and LAN discovery only exist natively. The browser client
// dials over websockets and never listens, so the wasm sibling exposes the
// same item names and signatures over a no-op behaviour.
#[cfg_attr(target_arch = "wasm32", path = "nat_wasm.rs")]
mod nat;
pub(crate) mod stats;
#[cfg(not(target_arch = "wasm32"))]
#[allow(unreachable_pub)]
mod storer;
pub(crate) mod task;

pub use base::BaseNode;
#[cfg(not(target_arch = "wasm32"))]
pub use bootnode::{BootNode, BootNodeBuilder};
pub use builder::BuiltInfrastructure;
pub use client::{ClientNode, ClientNodeBuilder};
#[cfg(feature = "swap")]
pub use core::SwapWiring;
pub use core::{
    ClientCore, ClientCoreCtx, PseudosettleWiring, SharedAccounting, assemble_client_core,
    spawn_client_command_bridge,
};
pub use error::NodeBuildError;
#[cfg(feature = "swap")]
pub use launch::LauncherSwapConfig;
pub use launch::{ClientLauncher, LaunchedClient};
#[cfg(not(target_arch = "wasm32"))]
pub use storer::{StorerNode, StorerNodeBuilder, StorerPullsyncControl};
