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
//! # Node Context
//!
//! The [`NodeContext`] provides infrastructure to protocols during launch:
//! - Task executor for spawning background tasks
//! - Data directory for persistent storage
//! - Shutdown signal for graceful termination
//!
//! # Configuration Traits
//!
//! Infrastructure configuration is defined via traits (all prefixed with `Node`):
//! - [`NodeRpcConfig`] - RPC server configuration (addresses, ports)
//! - [`NodeMetricsConfig`] - Metrics endpoint configuration
//! - [`NodeLoggingConfig`] - Logging format and rotation configuration
//! - [`NodeDatabaseConfig`] - Database storage configuration
//! - [`NodeConfig`] - Combined infrastructure configuration
//!
//! # Example
//!
//! ```ignore
//! use vertex_node_api::{NodeProtocol, NodeContext};
//!
//! // Node builder creates context with infrastructure
//! let ctx = NodeContext::new(executor, data_dir);
//!
//! // Launch builds and spawns in one step
//! let components = SwarmProtocol::<MyConfig>::launch(config, &ctx, &executor).await?;
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
