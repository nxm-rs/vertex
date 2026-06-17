//! Swarm redistribution (storage incentives) configuration.
//!
//! This crate provides configuration for the Swarm redistribution game,
//! which incentivizes nodes to store and serve chunks within their
//! neighborhood of responsibility.

mod args;
mod config;
mod redistribution;

pub use args::RedistributionArgs;
pub use config::StorageConfig;
pub use redistribution::{Entitlement, canonical_neighbourhood, sample};
