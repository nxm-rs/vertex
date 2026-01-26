//! Generic node infrastructure library.
//!
//! This crate provides protocol-agnostic node infrastructure:
//! - [`args`] - CLI argument structs for infrastructure configuration
//! - [`config`] - Generic configuration loading with protocol parameterization
//! - [`dirs`] - Data directory management
//! - [`logging`] - Logging initialization
//! - [`version`] - Version information
//!
//! For Swarm-specific builders, see `vertex-swarm-builder`.
//! For node launch patterns, see `vertex-node-builder`.

pub mod args;
pub mod config;
pub mod constants;
pub mod dirs;
pub mod logging;
pub mod version;
