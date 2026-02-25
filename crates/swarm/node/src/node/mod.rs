//! Node types for Swarm network participation.
//!
//! - [`BootNode`] - Topology only (bootnode servers)
//! - [`ClientNode`] - Topology + client protocols (chunk read/write)
//! - [`StorerNode`] - Client + storage protocols (full node)

mod base;
mod bootnode;
mod builder;
mod client;
mod error;
mod storer;

pub use base::BaseNode;
pub use bootnode::{BootNode, BootNodeBuilder, BootnodeBehaviour, BootnodeEvent};
pub use builder::BuiltInfrastructure;
pub use client::{ClientNode, ClientNodeBehaviour, ClientNodeBuilder, ClientNodeEvent};
pub use error::NodeBuildError;
pub use storer::{StorerNode, StorerNodeBuilder};
