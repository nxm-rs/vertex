//! Node API - Protocol and infrastructure traits for Vertex nodes.
//!
//! This crate provides the core abstractions for running network protocols
//! on node infrastructure. It is protocol-agnostic and does not depend on
//! any specific protocol implementation.
//!
//! # Protocol Trait
//!
//! The [`NodeProtocol`] trait defines the lifecycle interface between a network
//! protocol (like Swarm) and the node infrastructure. A single `launch()` method
//! handles building components and spawning services.
//!
//! # Infrastructure Context
//!
//! The [`InfrastructureContext`] trait provides protocols with access to shared
//! node infrastructure during launch:
//! - Task executor for spawning background tasks
//! - Data directory for persistent storage
//!
//! # Configuration Traits
//!
//! Infrastructure configuration is defined via traits (all prefixed with `Node`):
//! - [`NodeRpcConfig`] - RPC server configuration (addresses, ports)
//! - [`NodeDatabaseConfig`] - Database storage configuration
//! - [`NodeConfig`] - Combined infrastructure configuration
//!
//! # Example
//!
//! ```ignore
//! use vertex_node_api::NodeProtocol;
//!
//! // Launch builds and spawns in one step
//! let components = SwarmProtocol::<MyConfig>::launch(config, &ctx).await?;
//!
//! // Components remain available for queries and RPC
//! ```

#![warn(missing_docs)]

mod config;
mod context;
mod protocol;

pub use config::*;
pub use context::*;
pub use protocol::*;
