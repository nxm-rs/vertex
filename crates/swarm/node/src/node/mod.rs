//! Node types for Swarm network participation.
//!
//! - [`BootNode`] - Topology only (bootnode servers)
//! - [`ClientNode`] - Topology + client protocols (chunk read/write)
//! - [`StorerNode`] - Client + storage protocols (full node)

mod base;
#[allow(unreachable_pub)]
mod bootnode;
mod builder;
#[allow(unreachable_pub)]
mod client;
mod error;
pub(crate) mod stats;
mod storer;
pub(crate) mod task;

pub use base::BaseNode;
pub use bootnode::{BootNode, BootNodeBuilder};
pub use builder::BuiltInfrastructure;
pub use client::{ClientNode, ClientNodeBuilder};
pub use error::NodeBuildError;
pub use storer::{StorerNode, StorerNodeBuilder};
