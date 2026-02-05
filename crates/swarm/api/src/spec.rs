//! Network specification trait for Swarm.

use alloc::{string::String, sync::Arc, vec::Vec};
use alloy_chains::Chain;
use alloy_primitives::Address;
use nectar_primitives::ChunkTypeSet;
use nectar_swarms::{NamedSwarm, Swarm};
use vertex_swarm_forks::{ForkCondition, ForkDigest, SwarmHardfork, SwarmHardforks};

/// Token trait for Swarm network tokens (BZZ variants).
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmToken: Send + Sync {
    /// Token contract address.
    fn address(&self) -> Address;

    /// Token name (e.g., "Swarm").
    fn name(&self) -> &str;

    /// Token symbol (e.g., "xBZZ", "sBZZ", "dBZZ").
    fn symbol(&self) -> &str;

    /// Decimal places.
    fn decimals(&self) -> u8;
}

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
pub trait SwarmSpec: Send + Sync + 'static {
    /// The set of chunk types supported by this network.
    type ChunkSet: ChunkTypeSet;

    /// The token type for this network specification.
    type Token: SwarmToken;

    /// Returns the corresponding Swarm network identifier.
    fn swarm(&self) -> Swarm;

    /// Returns the underlying blockchain this network uses.
    fn chain(&self) -> Chain;

    /// Returns the network ID for the Swarm network.
    fn network_id(&self) -> u64 {
        self.swarm().id()
    }

    /// Returns the Swarm network name (like "mainnet", "testnet", etc.).
    fn network_name(&self) -> &str;

    /// Returns the bootnodes for the network (as multiaddr strings).
    ///
    /// Consumers should parse these as `Multiaddr` in the networking layer.
    fn bootnodes(&self) -> Option<Vec<String>>;

    /// Returns the Swarm token details.
    ///
    /// This defines which BZZ token this network uses and where it's deployed.
    fn token(&self) -> &Self::Token;

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
            if let ForkCondition::Timestamp(activation) = condition
                && activation > after
            {
                return Some(activation);
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
        self.network_id() == NamedSwarm::Mainnet as u64
    }

    /// Returns whether this is a testnet Swarm.
    fn is_testnet(&self) -> bool {
        self.network_id() == NamedSwarm::Testnet as u64
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

/// Trait for types that can provide a SwarmSpec.
///
/// Useful for dependency injection and testing.
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
