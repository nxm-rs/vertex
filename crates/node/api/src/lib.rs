//! Node API - Component containers and configuration traits for Swarm nodes.
//!
//! Provides runtime containers that hold SwarmTypes instances:
//! - [`LightComponents`] - Read-only (SwarmReader)
//! - [`PublisherComponents`] - Can upload (SwarmWriter)
//! - [`FullComponents`] - Stores and syncs
//!
//! And configuration traits for node infrastructure:
//! - [`RpcConfig`] - gRPC server configuration
//! - [`MetricsConfig`] - Metrics endpoint configuration
//! - [`LoggingConfig`] - Logging and log rotation configuration
//! - [`DatabaseConfig`] - Database storage configuration

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]

extern crate alloc;

mod components;
mod config;

pub use components::*;
pub use config::*;
