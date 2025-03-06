//! Swarm network specification for the Vertex node
//!
//! This crate defines the specifications for a Swarm network,
//! including chain identifiers, protocol parameters, and fork schedules.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

mod constants;
mod forks;
mod network;
mod types;

pub use constants::*;
pub use forks::*;
pub use network::*;
pub use types::*;

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};
use vertex_primitives::network::Swarm;

/// A counter for generating unique network IDs for development/testing
static DEV_NETWORK_ID_COUNTER: AtomicU64 = AtomicU64::new(1337);

/// Generate a unique network ID for development purposes
///
/// This ensures that development/test networks don't clash with each other
pub fn generate_dev_network_id() -> u64 {
    DEV_NETWORK_ID_COUNTER.fetch_add(1, Ordering::SeqCst)
}

/// The network specification trait that all Swarm network implementations
/// must satisfy.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmSpec: Send + Sync + 'static {
    /// Returns the corresponding Swarm network identifier
    fn swarm(&self) -> Swarm;

    /// Returns the network ID for the Swarm network
    fn network_id(&self) -> u64;

    /// Returns the Swarm network name (like "mainnet", "testnet", etc.)
    fn network_name(&self) -> &str;

    /// Returns the bootnodes for the network
    fn bootnodes(&self) -> &[Multiaddr];

    /// Returns the fork activation status for a given Swarm hardfork at a timestamp
    fn is_fork_active_at_timestamp(&self, fork: SwarmHardfork, timestamp: u64) -> bool;

    /// Returns whether this is the mainnet Swarm
    fn is_mainnet(&self) -> bool {
        self.network_id() == 1
    }

    /// Returns whether this is a testnet Swarm
    fn is_testnet(&self) -> bool {
        self.network_id() == 10
    }
}
