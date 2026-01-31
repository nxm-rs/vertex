//! Swarm business logic and orchestration (libp2p-FREE).
//!
//! This crate provides configuration and CLI parsing for Swarm nodes.
//! All libp2p networking is handled by `vertex-client-core`.
//!
//! # Architecture
//!
//! ```text
//! vertex-swarm-core (THIS CRATE - libp2p-FREE)
//! ├── CLI configuration (args/, config.rs)
//! └── Re-exports from vertex-client-core
//!
//! vertex-client-core (THE LIBP2P BOUNDARY)
//! ├── SwarmNode: wraps libp2p::Swarm
//! ├── NodeBehaviour: composed NetworkBehaviour
//! ├── ClientService: event processing
//! └── PeerId ↔ OverlayAddress translation
//! ```
//!
//! # Usage
//!
//! ```ignore
//! use vertex_swarm_core::{SwarmNode, ClientService, ClientHandle};
//!
//! // Build node with network config
//! let (mut node, client_service, client_handle) = SwarmNode::<MyNodeTypes>::builder(identity)
//!     .with_network_config(&my_network_args)
//!     .build()
//!     .await?;
//!
//! // Spawn client service
//! tokio::spawn(client_service.run());
//!
//! // Run node event loop
//! node.run().await?;
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "cli")]
pub mod args;
#[cfg(feature = "cli")]
mod config;
#[cfg(feature = "cli")]
mod constants;

// Re-export everything from vertex-client-core for backwards compatibility
pub use vertex_client_core::{
    BootNode, BootNodeBuilder, BootnodeBehaviour, BootnodeClient, BootnodeEvent, BootnodeProvider,
    BuiltSwarmComponents, Client, ClientCommand, ClientEvent, ClientHandle, ClientService,
    FullClient, NodeEvent, RetrievalError, RetrievalResult, SwarmNode, SwarmNodeBehaviour,
    SwarmNodeBuilder,
};

pub use vertex_swarm_primitives::SwarmNodeType;

// Re-export from vertex-client-core
pub use vertex_client_core::{spawn_stats_task, StatsConfig};

#[cfg(feature = "cli")]
pub use config::SwarmConfig;
