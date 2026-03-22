//! Swarm node builder infrastructure.
//!
//! Provides a progressive type-state builder chain for constructing Swarm nodes:
//! - [`SwarmBaseBuilder`] / [`DefaultBaseBuilder`] - Bootnode builder
//! - [`SwarmClientBuilder`] / [`DefaultClientBuilder`] - Client node builder
//! - [`SwarmStorerBuilder`] / [`DefaultStorerBuilder`] - Storer node builder
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
    DefaultBaseBuilder, DefaultClientBuilder, DefaultStorerBuilder, SwarmBaseBuilder,
    SwarmClientBuilder, SwarmProtocolBuilder, SwarmStorerBuilder, SwarmWithIdentity, SwarmWithSpec,
};

// Build outputs
pub use handle::{BuiltBootnode, BuiltClient, BuiltNode, BuiltStorer};

// Providers
pub use providers::NetworkChunkProvider;
pub use rpc::{BootnodeRpcProviders, FullRpcProviders, SwarmNodeProviders};

// Configs
pub use config::{SwarmBuildConfig, SwarmConfigError};

// Launch types (for SwarmLaunchConfig associated types)
pub use launch::{BootnodeLaunchTypes, ClientLaunchTypes};

// Errors
pub use error::SwarmNodeError;

// Re-exports
pub use vertex_swarm_api::{BootnodeComponents, ClientComponents, StorerComponents};
pub use vertex_swarm_bandwidth::{AccountingBuilder, NoAccountingBuilder};
