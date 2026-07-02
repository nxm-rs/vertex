//! Protocol and infrastructure traits for Vertex nodes.
//!
//! This crate defines the generic node infrastructure that any network protocol
//! can plug into. It is deliberately protocol-agnostic: it knows nothing about
//! Swarm, libp2p, or any specific network.
//!
//! # Key Traits
//!
//! - [`NodeProtocol`] - lifecycle trait for a network protocol. Receives an
//!   [`InfrastructureContext`] (executor + data directory), builds components,
//!   spawns services, and returns components for RPC/metrics.
//! - [`NodeBuildsProtocol`] - config-to-protocol mapping. Enables type inference
//!   at `with_protocol()` so the builder knows which protocol to construct.
//! - [`InfrastructureContext`] - provides a [`TaskExecutor`](vertex_tasks::TaskExecutor)
//!   and data directory to protocols during launch.
//! - [`NodeProtocolConfig`] - protocol-specific configuration with CLI argument
//!   support.

#![warn(missing_docs)]

mod config;
mod context;
mod protocol;

pub use config::*;
pub use context::*;
pub use protocol::*;
