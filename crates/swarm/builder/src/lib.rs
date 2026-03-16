//! Swarm node builder infrastructure.
//!
//! Provides layered builders for constructing Swarm nodes:
//! - [`NodeBuilder`] / [`DefaultNodeBuilder`] - Bootnode builder
//! - [`ClientNodeBuilder`] / [`DefaultClientBuilder`] - Client node builder
//! - [`StorerNodeBuilder`] / [`DefaultStorerBuilder`] - Storer node builder
//!
//! Build returns [`BuiltNode`] which contains the task and RPC providers.

pub mod config;
mod error;
mod handle;
mod launch;
mod node;
mod providers;
mod rpc;

// Traits
pub use node::BuilderExt;

// Builders
pub use node::{
    ClientNodeBuilder, DefaultClientBuilder, DefaultNodeBuilder, DefaultStorerBuilder,
    NodeBuilder, StorerNodeBuilder,
};

// Build outputs
pub use handle::{BuiltBootnode, BuiltClient, BuiltNode, BuiltStorer};

// Providers
pub use providers::NetworkChunkProvider;
pub use rpc::{BootnodeRpcProviders, ClientRpcProviders, StorerRpcProviders};

// Configs
pub use config::{BootnodeConfig, ClientConfig, StorerConfig};

// Launch types (for SwarmLaunchConfig associated types)
pub use launch::{BootnodeLaunchTypes, ClientLaunchTypes, StorerLaunchTypes};

// Errors
pub use error::SwarmNodeError;

// Re-exports
pub use vertex_swarm_api::{BootnodeComponents, ClientComponents, StorerComponents};
pub use vertex_swarm_bandwidth::{AccountingBuilder, NoAccountingBuilder};
