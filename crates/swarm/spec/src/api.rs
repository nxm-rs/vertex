//! The SwarmSpec trait and related abstractions
//!
//! This module defines [`SwarmSpec`], the core trait that any Swarm network
//! specification must implement.
//!
//! # Design
//!
//! `SwarmSpec` is intentionally minimal. It provides only what's needed to
//! identify a network and determine protocol behavior at any point in time:
//!
//! - **Identity**: network ID, name, underlying chain
//! - **Discovery**: bootstrap nodes for joining the network
//! - **Protocol**: hardfork schedule determining feature activation
//! - **Economics**: which BZZ token contract the network uses
//!
//! The trait is generic over implementation - code accepting `impl SwarmSpec`
//! works with mainnet, testnet, or custom test networks without modification.
//!
//! # Example
//!
//! ```ignore
//! use vertex_swarmspec::{SwarmSpec, Hive};
//!
//! fn connect_to_network<S: SwarmSpec>(spec: &S) {
//!     println!("Joining {} (network ID: {})", spec.network_name(), spec.network_id());
//!     if let Some(bootnodes) = spec.bootnodes() {
//!         // Connect to bootstrap nodes
//!     }
//! }
//! ```

use crate::{
    Hive, Token,
    constants::{mainnet, testnet},
};
use alloc::{string::String, sync::Arc, vec::Vec};
use alloy_chains::Chain;
use core::fmt::Debug;
use nectar_primitives::{ChunkTypeSet, StandardChunkSet};
use nectar_swarms::Swarm;
use vertex_swarm_forks::{ForkCondition, ForkDigest, SwarmHardfork, SwarmHardforks};

/// A Swarm network specification.
///
/// Defines the consensus-critical parameters that identify a network and
/// govern protocol behavior. All nodes on the same network must use an
/// equivalent spec to interoperate.
///
/// Covers network-level concerns fixed for all participants:
/// - Network identity (ID, name, underlying L1 chain)
/// - Bootstrap nodes for initial peer discovery
/// - Hardfork activation schedule
/// - Token contract address
/// - Supported chunk types
///
/// Excludes node-level config (storage capacity, pricing, cache policies).
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmSpec: Send + Sync + Unpin + Debug + 'static {
    /// The set of chunk types supported by this network.
    type ChunkSet: ChunkTypeSet;

    /// Returns the corresponding Swarm network identifier.
    fn swarm(&self) -> Swarm;

    /// Returns the underlying blockchain this network uses.
    fn chain(&self) -> Chain;

    /// Returns the network ID for the Swarm network.
    fn network_id(&self) -> u64;

    /// Returns the Swarm network name (like "mainnet", "testnet", etc.).
    fn network_name(&self) -> &str;

    /// Returns the bootnodes for the network (as multiaddr strings).
    ///
    /// Consumers should parse these as `Multiaddr` in the networking layer.
    fn bootnodes(&self) -> Option<Vec<String>>;

    /// Returns the Swarm token details.
    ///
    /// This defines which BZZ token this network uses and where it's deployed.
    fn token(&self) -> &Token;

    /// Returns the hardforks configuration.
    fn hardforks(&self) -> &SwarmHardforks;

    /// Returns the chunk size in bytes for this network.
    ///
    /// This is the fundamental unit of storage in Swarm. Standard Swarm
    /// networks use 4096 bytes (2^12), but custom networks may differ.
    ///
    /// The default implementation returns the chunk size from the associated
    /// `ChunkSet` type, providing compile-time access to this value.
    fn chunk_size(&self) -> usize {
        Self::ChunkSet::BODY_SIZE
    }

    /// Returns the reserve capacity in number of chunks for full nodes.
    ///
    /// This is the target number of chunks a full storage node should hold.
    /// Standard networks use 2^22 = 4,194,304 chunks.
    fn reserve_capacity(&self) -> u64;

    /// Returns the fork activation status for a given Swarm hardfork at a timestamp.
    fn is_fork_active_at_timestamp(&self, fork: SwarmHardfork, timestamp: u64) -> bool;

    /// Returns the activation timestamp of the next fork after the given timestamp.
    ///
    /// Used during handshake to communicate upcoming protocol changes.
    /// Returns `None` if all known forks are already active.
    fn next_fork_timestamp(&self, after: u64) -> Option<u64> {
        for (_, condition) in self.hardforks().forks_iter() {
            if let ForkCondition::Timestamp(activation) = condition {
                if activation > after {
                    return Some(activation);
                }
            }
        }
        None
    }

    /// Computes a digest representing the current fork state at a given timestamp.
    ///
    /// Two nodes with the same digest are fork-compatible and can interoperate.
    /// The digest incorporates network ID, genesis timestamp, and active forks.
    ///
    /// During handshake, peers exchange digests to verify compatibility.
    fn fork_digest(&self, at_timestamp: u64) -> ForkDigest;

    /// Returns whether this is the mainnet Swarm.
    fn is_mainnet(&self) -> bool {
        self.network_id() == mainnet::NETWORK_ID
    }

    /// Returns whether this is a testnet Swarm.
    fn is_testnet(&self) -> bool {
        self.network_id() == testnet::NETWORK_ID
    }

    /// Returns the maximum proximity order for addresses in this network.
    fn max_po(&self) -> u8 {
        nectar_primitives::MAX_PO
    }

    /// Returns whether this is a development network.
    fn is_dev(&self) -> bool {
        !self.is_mainnet() && !self.is_testnet()
    }
}

impl SwarmSpec for Hive {
    type ChunkSet = StandardChunkSet;

    fn swarm(&self) -> Swarm {
        match self.network_id {
            mainnet::NETWORK_ID => nectar_swarms::NamedSwarm::Mainnet.into(),
            testnet::NETWORK_ID => nectar_swarms::NamedSwarm::Testnet.into(),
            _ => Swarm::from_id(self.network_id),
        }
    }

    fn chain(&self) -> Chain {
        self.chain
    }

    fn network_id(&self) -> u64 {
        self.network_id
    }

    fn network_name(&self) -> &str {
        &self.network_name
    }

    fn bootnodes(&self) -> Option<Vec<String>> {
        if self.bootnodes.is_empty() {
            None
        } else {
            Some(self.bootnodes.clone())
        }
    }

    fn token(&self) -> &Token {
        &self.token
    }

    fn hardforks(&self) -> &SwarmHardforks {
        &self.hardforks
    }

    fn reserve_capacity(&self) -> u64 {
        self.reserve_capacity
    }

    fn is_fork_active_at_timestamp(&self, fork: SwarmHardfork, timestamp: u64) -> bool {
        match self.hardforks.get(fork) {
            Some(ForkCondition::Timestamp(activation_time)) => timestamp >= activation_time,
            _ => false,
        }
    }

    fn fork_digest(&self, at_timestamp: u64) -> ForkDigest {
        // Collect active fork timestamps
        let active_forks: Vec<u64> = self
            .hardforks
            .forks_iter()
            .filter_map(|(_, condition)| {
                if let ForkCondition::Timestamp(activation) = condition {
                    if activation <= at_timestamp {
                        return Some(activation);
                    }
                }
                None
            })
            .collect();

        ForkDigest::compute(self.network_id, self.genesis_timestamp, &active_forks)
    }
}

/// Trait for types that can provide a SwarmSpec.
///
/// This is useful for dependency injection and testing.
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait SwarmSpecProvider: Send + Sync {
    /// The spec type this provider returns.
    type Spec: SwarmSpec;

    /// Get a reference to the spec.
    fn spec(&self) -> &Self::Spec;
}

/// A simple provider that wraps a spec.
#[derive(Debug, Clone)]
pub struct StaticSwarmSpecProvider<S: SwarmSpec> {
    spec: Arc<S>,
}

impl<S: SwarmSpec> StaticSwarmSpecProvider<S> {
    /// Create a new provider with the given spec.
    pub fn new(spec: S) -> Self {
        Self {
            spec: Arc::new(spec),
        }
    }

    /// Create a new provider from an Arc.
    pub fn from_arc(spec: Arc<S>) -> Self {
        Self { spec }
    }
}

impl<S: SwarmSpec> SwarmSpecProvider for StaticSwarmSpecProvider<S> {
    type Spec = S;

    fn spec(&self) -> &Self::Spec {
        &self.spec
    }
}

// Also implement SwarmSpecProvider for Arc<Hive> directly
impl SwarmSpecProvider for Arc<Hive> {
    type Spec = Hive;

    fn spec(&self) -> &Self::Spec {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HiveBuilder, init_mainnet, init_testnet};

    #[test]
    fn test_swarm_spec_trait() {
        // Verify Hive implements SwarmSpec
        fn assert_spec<S: SwarmSpec>(_s: &S) {}

        let mainnet = init_mainnet();
        assert_spec(&*mainnet);

        let testnet = init_testnet();
        assert_spec(&*testnet);
    }

    #[test]
    fn test_network_checks() {
        let mainnet = init_mainnet();
        assert!(mainnet.is_mainnet());
        assert!(!mainnet.is_testnet());
        assert!(!mainnet.is_dev());

        let testnet = init_testnet();
        assert!(!testnet.is_mainnet());
        assert!(testnet.is_testnet());
        assert!(!testnet.is_dev());

        let dev = HiveBuilder::dev().build();
        assert!(!dev.is_mainnet());
        assert!(!dev.is_testnet());
        assert!(dev.is_dev());
    }

    #[test]
    fn test_spec_provider() {
        let spec = init_mainnet();
        let provider: StaticSwarmSpecProvider<Hive> =
            StaticSwarmSpecProvider::from_arc(spec.clone());

        assert_eq!(provider.spec().network_id(), spec.network_id());
    }

    #[test]
    fn test_hardforks() {
        let spec = init_mainnet();
        let hardforks = spec.hardforks();

        // Test that we can query hardforks
        let genesis = hardforks.get(SwarmHardfork::Genesis);
        assert!(matches!(genesis, Some(ForkCondition::Timestamp(_))));
    }

    #[test]
    fn test_fork_digest() {
        let mainnet = init_mainnet();
        let testnet = init_testnet();

        // Same network at same time should produce same digest
        let digest1 = mainnet.fork_digest(1000000);
        let digest2 = mainnet.fork_digest(1000000);
        assert_eq!(digest1, digest2);

        // Different networks should produce different digests
        let mainnet_digest = mainnet.fork_digest(1000000);
        let testnet_digest = testnet.fork_digest(1000000);
        assert_ne!(mainnet_digest, testnet_digest);

        // Digest display works
        let digest = mainnet.fork_digest(1000000);
        let display = format!("{}", digest);
        assert!(display.starts_with("0x"));
        assert_eq!(display.len(), 10); // "0x" + 8 hex chars
    }

    #[test]
    fn test_fork_digest_changes_with_active_forks() {
        // Build two specs with different fork activation times
        let spec1 = HiveBuilder::new()
            .network_id(100)
            .with_accord(1000)
            .genesis_timestamp(0)
            .build();

        let spec2 = HiveBuilder::new()
            .network_id(100)
            .with_accord(2000)
            .genesis_timestamp(0)
            .build();

        // Before any fork is active, digests should differ only by fork timestamps in hash
        // At timestamp 500, neither fork is active
        let d1_before = spec1.fork_digest(500);
        let d2_before = spec2.fork_digest(500);
        assert_eq!(d1_before, d2_before); // Same because no forks active

        // At timestamp 1500, spec1's accord is active but not spec2's
        let d1_after = spec1.fork_digest(1500);
        let d2_after = spec2.fork_digest(1500);
        assert_ne!(d1_after, d2_after); // Different because different forks active
    }

    #[test]
    fn test_next_fork_timestamp() {
        // Build a spec with a future fork
        let spec = HiveBuilder::new()
            .network_id(100)
            .with_accord(1000)
            .genesis_timestamp(0)
            .build();

        // Before the fork, next_fork_timestamp should return the fork time
        assert_eq!(spec.next_fork_timestamp(0), Some(1000));
        assert_eq!(spec.next_fork_timestamp(500), Some(1000));

        // After/at the fork, no more forks
        assert_eq!(spec.next_fork_timestamp(1000), None);
        assert_eq!(spec.next_fork_timestamp(2000), None);
    }
}
