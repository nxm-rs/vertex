//! Network specification traits for Swarm.

use alloc::{string::String, sync::Arc, vec::Vec};
use alloy_chains::Chain;
use alloy_primitives::Address;
use nectar_primitives::ChunkTypeSet;
use nectar_swarms::{NamedSwarm, Swarm};
use vertex_swarm_forks::{ForkCondition, ForkDigest, SwarmHardfork, SwarmHardforks};

/// Parser for Swarm network specifications.
///
/// Handles both preset names ("mainnet", "testnet") and file paths via a single
/// `parse()` method.
pub trait SwarmSpecParser: Clone + Send + Sync + 'static {
    /// The spec type this parser produces.
    type Spec: SwarmSpec + Send + Sync;

    /// Supported preset network names.
    const SUPPORTED_NETWORKS: &'static [&'static str];

    /// Parse a spec from a preset name or file path.
    fn parse(s: &str) -> eyre::Result<Arc<Self::Spec>>;
}

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

/// Consensus-critical parameters that identify a Swarm network.
///
/// All nodes on the same network must use an equivalent spec.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmSpec: Send + Sync + 'static {
    /// Supported chunk types.
    type ChunkSet: ChunkTypeSet;

    /// Token type for this network.
    type Token: SwarmToken;

    /// Swarm network identifier.
    fn swarm(&self) -> Swarm;

    /// Underlying L1 chain.
    fn chain(&self) -> Chain;

    /// Numeric network ID.
    fn network_id(&self) -> u64 {
        self.swarm().id()
    }

    /// Network name (e.g. "mainnet", "testnet").
    fn network_name(&self) -> &str;

    /// Bootstrap node multiaddrs (as strings).
    fn bootnodes(&self) -> Option<Vec<String>>;

    /// Token details (contract address, symbol, decimals).
    fn token(&self) -> &Self::Token;

    /// Hardfork activation schedule.
    fn hardforks(&self) -> &SwarmHardforks;

    /// Chunk body size in bytes. Defaults to the `ChunkSet` constant.
    fn chunk_size(&self) -> usize {
        Self::ChunkSet::BODY_SIZE
    }

    /// Target reserve capacity in chunks for full nodes.
    fn reserve_capacity(&self) -> u64;

    /// Whether `fork` is active at `timestamp`.
    fn is_fork_active_at_timestamp(&self, fork: SwarmHardfork, timestamp: u64) -> bool;

    /// Activation timestamp of the next fork after `after`, if any.
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

    /// Fork-compatibility digest at a given timestamp.
    fn fork_digest(&self, at_timestamp: u64) -> ForkDigest;

    /// Whether this is the mainnet Swarm.
    fn is_mainnet(&self) -> bool {
        self.network_id() == NamedSwarm::Mainnet as u64
    }

    /// Whether this is a testnet Swarm.
    fn is_testnet(&self) -> bool {
        self.network_id() == NamedSwarm::Testnet as u64
    }

    /// Base price per chunk in Accounting Units (AU).
    fn base_price(&self) -> u64 {
        10_000
    }

    /// Maximum proximity order for addresses in this network.
    fn max_po(&self) -> u8 {
        nectar_primitives::MAX_PO
    }

    /// Whether this is a development network.
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
