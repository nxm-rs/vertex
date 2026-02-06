//! Swarm node builder infrastructure.
//!
//! Provides layered builders for constructing Swarm nodes:
//! - [`NodeBuilder`] - Foundation for all nodes, usable as bootnode
//! - [`ClientNodeBuilder`] - Extends base with bandwidth accounting
//! - [`StorerNodeBuilder`] - Extends client with local storage
//!
//! Type aliases for common configurations:
//! - [`DefaultNodeBuilder`] - NodeBuilder with Arc<Identity> and NetworkConfig
//! - [`DefaultClientBuilder`] - ClientNodeBuilder with standard types
//! - [`DefaultStorerBuilder`] - StorerNodeBuilder with standard types

pub mod config;
mod error;
mod node;
mod providers;
mod rpc;

// Node building - layered generic API
pub use error::SwarmNodeError;
pub use node::{
    BaseNodeHandle, ClientNodeBuilder, ClientNodeHandle, NodeBuilder, StorerNodeBuilder,
    StorerNodeHandle,
};
// Type aliases for default configurations
pub use node::{DefaultClientBuilder, DefaultNodeBuilder, DefaultStorerBuilder};
pub use providers::NetworkChunkProvider;
pub use rpc::{BootnodeRpcProviders, ClientRpcProviders, StorerRpcProviders};

// Config types
pub use config::{BootnodeConfig, ClientConfig, StorerConfig};

// Re-export AccountingBuilder from bandwidth crate
pub use vertex_swarm_bandwidth::{AccountingBuilder, NoAccountingBuilder};

// Re-export component types from API crate
pub use vertex_swarm_api::{BootnodeComponents, ClientComponents, StorerComponents};
