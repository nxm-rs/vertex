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
//! - [`Spec`] - Concrete spec implementation for mainnet, testnet, and dev networks
//! - [`SpecBuilder`] - Constructs custom specs for testing or private networks
//!
//! # Example
//!
//! ```ignore
//! use vertex_swarm_spec::{init_mainnet, SwarmSpec};
//!
//! let spec = init_mainnet();
//! assert!(spec.is_mainnet());
//!
//! // Query protocol state at a given time
//! if spec.is_fork_active_at_timestamp(SwarmHardfork::Genesis, timestamp) {
//!     // Post-Genesis protocol behavior
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
#[cfg(feature = "std")]
mod parser;
mod spec;
mod token;

// Re-export chain types
pub use alloy_chains::{Chain, NamedChain};

// Re-export hardfork types
pub use vertex_swarm_forks::*;

// Re-export contract bindings and addresses from nectar
pub use nectar_contracts;

// Re-export SwarmSpec trait and providers from vertex-swarm-api
pub use vertex_swarm_api::{
    StaticSwarmSpecProvider, SwarmSpec, SwarmSpecParser, SwarmSpecProvider, SwarmToken,
};
pub use constants::*;
#[cfg(feature = "std")]
pub use display::Loggable;
pub use display::{DisplaySwarmSpec, SwarmSpecExt};
#[cfg(feature = "std")]
pub use error::SwarmSpecFileError;
pub use nectar_primitives::{ChunkTypeSet, StandardChunkSet};
#[cfg(feature = "std")]
pub use parser::DefaultSpecParser;
#[cfg(feature = "std")]
pub use spec::{DEV, MAINNET, TESTNET};
pub use spec::{Spec, SpecBuilder};
pub use token::Token;

// HasSpec trait is defined in this module and exported directly

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

/// Types that hold an `Arc<Spec>`.
///
/// Provides shared access to the network specification without transferring ownership.
/// Implement this for types that need to provide spec access to multiple consumers.
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait HasSpec: Send + Sync {
    /// Get the network specification.
    fn spec(&self) -> &Arc<Spec>;
}

/// A counter for generating unique network IDs for development/testing
static DEV_NETWORK_ID_COUNTER: AtomicU64 = AtomicU64::new(1337);

/// Generate a unique network ID for development purposes.
///
/// Ensures development/test networks don't clash with each other.
pub fn generate_dev_network_id() -> u64 {
    DEV_NETWORK_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Initialize and return the mainnet specification.
///
/// This lazily initializes the mainnet spec on first call and returns
/// a clone of the Arc on subsequent calls.
#[cfg(feature = "std")]
pub fn init_mainnet() -> Arc<Spec> {
    spec::init_mainnet()
}

/// Initialize and return the testnet specification.
///
/// This lazily initializes the testnet spec on first call and returns
/// a clone of the Arc on subsequent calls.
#[cfg(feature = "std")]
pub fn init_testnet() -> Arc<Spec> {
    spec::init_testnet()
}

/// Initialize and return the development network specification.
///
/// This lazily initializes the dev spec on first call and returns
/// a clone of the Arc on subsequent calls.
#[cfg(feature = "std")]
pub fn init_dev() -> Arc<Spec> {
    spec::init_dev()
}

/// Convenient re-exports for common usage patterns.
pub mod prelude {
    pub use super::{
        // Concrete type
        Spec,
        SpecBuilder,
        // Core traits
        SwarmSpec,
        SwarmSpecParser,
        SwarmSpecProvider,
    };

    #[cfg(feature = "std")]
    pub use super::{DefaultSpecParser, init_dev, init_mainnet, init_testnet};
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

        // Arc<Spec> implements SwarmSpecProvider
        assert_eq!(spec.spec().network_id(), mainnet::NETWORK_ID);

        // StaticSwarmSpecProvider also works
        let provider = StaticSwarmSpecProvider::from_arc(spec);
        assert_eq!(provider.spec().network_id(), mainnet::NETWORK_ID);
    }

    #[test]
    fn test_custom_network_chunk_config() {
        // Reserve capacity is configurable at runtime
        let custom = SpecBuilder::new()
            .network_id(999)
            .reserve_capacity(1 << 20)
            .build();

        // chunk_size is now a compile-time constant from ChunkSet::BODY_SIZE
        // Custom chunk sizes require implementing a different ChunkTypeSet
        assert_eq!(custom.chunk_size(), nectar_primitives::DEFAULT_BODY_SIZE);
        assert_eq!(custom.reserve_capacity(), 1 << 20);
    }
}
