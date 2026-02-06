//! Swarm redistribution (storage incentives) configuration.
//!
//! This crate provides configuration for the Swarm redistribution game,
//! which incentivizes nodes to store and serve chunks within their
//! neighborhood of responsibility.

mod args;
mod config;

pub use args::RedistributionArgs;
pub use config::StorageConfig;
