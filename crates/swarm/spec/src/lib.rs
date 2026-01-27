//! Swarm network specification
//!
//! This crate defines *what* a Swarm network is - its identity and protocol rules -
//! without prescribing *how* a node should operate on it.
//!
//! # Design Philosophy
//!
//! A network specification answers: "Which network am I connecting to?"
//!
//! It captures the immutable facts about a network that all participants must agree on:
//! network ID, hardfork schedule, token contract, and bootstrap nodes. Two nodes with
//! the same spec will join the same network; two nodes with different specs won't.
//!
//! What a spec deliberately excludes is node-level policy: how much storage to allocate,
//! bandwidth pricing, cache strategies. These vary per operator and belong in node
//! configuration, not network specification. This separation allows light clients and
//! full nodes to share the same spec while differing in operational parameters.
//!
//! # Core Types
//!
//! - [`SwarmSpec`] - Trait defining what a network specification must provide
//! - [`Hive`] - Concrete spec implementation for mainnet, testnet, and dev networks
//! - [`HiveBuilder`] - Constructs custom specs for testing or private networks
//!
//! # Example
//!
//! ```ignore
//! use vertex_swarmspec::{init_mainnet, SwarmSpec};
//!
//! let spec = init_mainnet();
//! assert!(spec.is_mainnet());
//!
//! // Query protocol state at a given time
//! if spec.is_fork_active_at_timestamp(SwarmHardfork::Accord, timestamp) {
//!     // Post-Accord protocol behavior
//! }
//! ```

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

mod api;
mod constants;
pub mod display;
mod error;
mod spec;
mod token;

// Re-export chain types
pub use alloy_chains::{Chain, NamedChain};

// Re-export hardfork types
pub use vertex_swarm_forks::*;

// Re-export contract bindings and addresses from nectar
pub use nectar_contracts;

pub use api::{StaticSwarmSpecProvider, SwarmSpec, SwarmSpecProvider};
#[cfg(feature = "std")]
pub use display::Loggable;
pub use display::{DisplaySwarmSpec, SwarmSpecExt};
pub use constants::*;
#[cfg(feature = "std")]
pub use error::SwarmSpecFileError;
pub use nectar_primitives::{ChunkTypeSet, StandardChunkSet};
pub use spec::{DEV, Hive, HiveBuilder, MAINNET, TESTNET};
pub use token::Token;

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

/// A counter for generating unique network IDs for development/testing
static DEV_NETWORK_ID_COUNTER: AtomicU64 = AtomicU64::new(1337);

/// Generate a unique network ID for development purposes.
///
/// Ensures development/test networks don't clash with each other.
pub fn generate_dev_network_id() -> u64 {
    DEV_NETWORK_ID_COUNTER.fetch_add(1, Ordering::SeqCst)
}

/// Initialize and return the mainnet specification.
///
/// This lazily initializes the mainnet spec on first call and returns
/// a clone of the Arc on subsequent calls.
pub fn init_mainnet() -> Arc<Hive> {
    spec::init_mainnet()
}

/// Initialize and return the testnet specification.
///
/// This lazily initializes the testnet spec on first call and returns
/// a clone of the Arc on subsequent calls.
pub fn init_testnet() -> Arc<Hive> {
    spec::init_testnet()
}

/// Initialize and return the development network specification.
///
/// This lazily initializes the dev spec on first call and returns
/// a clone of the Arc on subsequent calls.
pub fn init_dev() -> Arc<Hive> {
    spec::init_dev()
}

/// Convenient re-exports for common usage patterns.
pub mod prelude {
    pub use super::{
        // Concrete type
        Hive,
        HiveBuilder,
        // Core trait
        SwarmSpec,
        SwarmSpecProvider,
        // Initialization
        init_dev,
        init_mainnet,
        init_testnet,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mainnet_spec() {
        let spec = init_mainnet();
        assert!(spec.is_mainnet());
        assert_eq!(spec.network_id(), mainnet::NETWORK_ID);
    }

    #[test]
    fn test_testnet_spec() {
        let spec = init_testnet();
        assert!(spec.is_testnet());
        assert_eq!(spec.network_id(), testnet::NETWORK_ID);
    }

    #[test]
    fn test_dev_spec() {
        let spec = init_dev();
        assert!(spec.is_dev());
    }

    #[test]
    fn test_spec_provider() {
        let spec = init_mainnet();

        // Arc<Hive> implements SwarmSpecProvider
        assert_eq!(spec.spec().network_id(), mainnet::NETWORK_ID);

        // StaticSwarmSpecProvider also works
        let provider = StaticSwarmSpecProvider::from_arc(spec);
        assert_eq!(provider.spec().network_id(), mainnet::NETWORK_ID);
    }

    #[test]
    fn test_custom_network_chunk_config() {
        // Custom networks can override chunk protocol parameters
        let custom = HiveBuilder::new()
            .network_id(999)
            .chunk_size(8192)
            .reserve_capacity(1 << 20)
            .build();

        assert_eq!(custom.chunk_size(), 8192);
        assert_eq!(custom.reserve_capacity(), 1 << 20);
    }
}
