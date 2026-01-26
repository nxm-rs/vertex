//! Node API - Protocol and infrastructure traits for Vertex nodes.
//!
//! This crate provides the core abstractions for running network protocols
//! on node infrastructure. It is protocol-agnostic and does not depend on
//! any specific protocol implementation.
//!
//! # Protocol Trait
//!
//! The [`Protocol`] trait defines the lifecycle interface between a network
//! protocol (like Swarm) and the node infrastructure:
//!
//! 1. **Build**: Create components and services from config + infrastructure
//! 2. **Run**: Start services using the task executor
//!
//! # Node Context
//!
//! The [`NodeContext`] provides infrastructure to protocols during build:
//! - Task executor for spawning background tasks
//! - Data directory for persistent storage
//! - Shutdown signal for graceful termination
//!
//! # Configuration Traits
//!
//! Infrastructure configuration is defined via traits:
//! - [`RpcConfig`] - RPC server configuration (addresses, ports)
//! - [`MetricsConfig`] - Metrics endpoint configuration
//! - [`LoggingConfig`] - Logging format and rotation configuration
//! - [`DatabaseConfig`] - Database storage configuration
//! - [`NodeConfig`] - Combined infrastructure configuration
//!
//! # Example
//!
//! ```ignore
//! use vertex_node_api::{Protocol, NodeContext, Built};
//!
//! // Node builder creates context with infrastructure
//! let ctx = NodeContext::new(executor, data_dir);
//!
//! // Protocol builds on the infrastructure
//! let built = SwarmLightProtocol::build(config, &ctx).await?;
//!
//! // Run protocol (services consumed, components returned)
//! let components = built.run(ctx.executor());
//! ```

#![warn(missing_docs)]

mod config;
mod context;
mod protocol;

pub use config::*;
pub use context::*;
pub use protocol::*;
