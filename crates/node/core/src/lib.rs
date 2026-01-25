//! Vertex Swarm node core library.
//!
//! This crate provides core node functionality:
//! - [`args`] - CLI argument structs for infrastructure configuration
//! - [`builder`] - Node builder for component assembly
//! - [`dirs`] - Data directory management
//! - [`logging`] - Logging initialization

pub mod args;
pub mod builder;
pub mod constants;
pub mod dirs;
pub mod logging;
pub mod version;
