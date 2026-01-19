//! Concrete network specifications
//!
//! This module provides [`Hive`], the standard implementation of [`SwarmSpec`]
//! used for mainnet, testnet, development, and custom networks.
//!
//! Pre-built specifications are available via [`init_mainnet`], [`init_testnet`],
//! and [`init_dev`]. Custom specifications can be constructed with [`HiveBuilder`].

use crate::{
    Token,
    constants::{dev, mainnet, testnet},
    generate_dev_network_id,
};
use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use alloy_chains::{Chain, NamedChain};
use libp2p::Multiaddr;
use vertex_net_primitives_traits::OnceLock;
use vertex_swarm_forks::{ForkCondition, SwarmHardfork, SwarmHardforks, SwarmHardforksTrait};

/// A concrete Swarm network specification.
///
/// `Hive` captures everything needed to identify and connect to a Swarm network:
/// which blockchain it settles on, how to find peers, when protocol upgrades
/// activate, and which token contract to use.
///
/// # Usage
///
/// For standard networks, use the provided initializers:
/// - [`init_mainnet()`] - Production network on Gnosis Chain
/// - [`init_testnet()`] - Test network on Sepolia
/// - [`init_dev()`] - Local development with auto-generated network ID
///
/// For custom networks, use [`HiveBuilder`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hive {
    /// Underlying blockchain
    pub chain: Chain,

    /// Network ID for this Swarm network
    pub network_id: u64,

    /// Network name (e.g., "mainnet", "testnet")
    pub network_name: String,

    /// Bootnodes - entry points into the network
    pub bootnodes: Vec<Multiaddr>,

    /// Hardforks configuration
    pub hardforks: SwarmHardforks,

    /// Swarm token details
    pub token: Token,

    /// Genesis timestamp (reference point for hardfork activation)
    pub genesis_timestamp: u64,
}

impl Default for Hive {
    fn default() -> Self {
        let mut hardforks = SwarmHardforks::new(vec![]);
        hardforks.insert(SwarmHardfork::Accord, ForkCondition::Timestamp(0));

        Self {
            chain: Chain::from(NamedChain::Dev),
            network_id: generate_dev_network_id(),
            network_name: dev::NETWORK_NAME.to_string(),
            bootnodes: Vec::new(),
            hardforks,
            token: dev::TOKEN,
            genesis_timestamp: 0,
        }
    }
}

/// The Swarm mainnet specification
pub static MAINNET: OnceLock<Arc<Hive>> = OnceLock::new();

/// Initialize the mainnet specification
pub(crate) fn init_mainnet() -> Arc<Hive> {
    MAINNET
        .get_or_init(|| {
            let mut hardforks = SwarmHardforks::new(vec![]);
            hardforks.insert(
                SwarmHardfork::Accord,
                ForkCondition::Timestamp(SwarmHardfork::MAINNET_GENESIS_TIMESTAMP),
            );

            let spec = Hive {
                chain: Chain::from(NamedChain::Gnosis),
                network_id: mainnet::NETWORK_ID,
                network_name: mainnet::NETWORK_NAME.to_string(),
                bootnodes: mainnet_bootnodes(),
                hardforks,
                token: mainnet::TOKEN,
                genesis_timestamp: SwarmHardfork::MAINNET_GENESIS_TIMESTAMP,
            };

            Arc::new(spec)
        })
        .clone()
}

/// The Swarm testnet specification
pub static TESTNET: OnceLock<Arc<Hive>> = OnceLock::new();

/// Initialize the testnet specification
pub(crate) fn init_testnet() -> Arc<Hive> {
    TESTNET
        .get_or_init(|| {
            let mut hardforks = SwarmHardforks::new(vec![]);
            hardforks.insert(
                SwarmHardfork::Accord,
                ForkCondition::Timestamp(SwarmHardfork::TESTNET_GENESIS_TIMESTAMP),
            );

            let spec = Hive {
                chain: Chain::from(NamedChain::Sepolia),
                network_id: testnet::NETWORK_ID,
                network_name: testnet::NETWORK_NAME.to_string(),
                bootnodes: testnet_bootnodes(),
                hardforks,
                token: testnet::TOKEN,
                genesis_timestamp: SwarmHardfork::TESTNET_GENESIS_TIMESTAMP,
            };

            Arc::new(spec)
        })
        .clone()
}

/// The Swarm development network specification
pub static DEV: OnceLock<Arc<Hive>> = OnceLock::new();

/// Initialize the dev specification
pub(crate) fn init_dev() -> Arc<Hive> {
    DEV.get_or_init(|| Arc::new(Hive::default())).clone()
}

/// Builder for constructing custom [`Hive`] specifications.
///
/// Start from scratch with [`HiveBuilder::new()`], or derive from an existing
/// network with [`HiveBuilder::mainnet()`], [`HiveBuilder::testnet()`], or
/// [`HiveBuilder::dev()`] and override specific fields.
#[derive(Debug, Default, Clone)]
pub struct HiveBuilder {
    chain: Option<Chain>,
    network_id: Option<u64>,
    network_name: Option<String>,
    bootnodes: Vec<Multiaddr>,
    hardforks: SwarmHardforks,
    token: Option<Token>,
    genesis_timestamp: Option<u64>,
}

impl HiveBuilder {
    /// Create a new specification builder
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the underlying blockchain
    pub fn chain(mut self, chain: Chain) -> Self {
        self.chain = Some(chain);
        self
    }

    /// Set the network ID
    pub fn network_id(mut self, network_id: u64) -> Self {
        self.network_id = Some(network_id);
        self
    }

    /// Set the network name
    pub fn network_name(mut self, name: impl ToString) -> Self {
        self.network_name = Some(name.to_string());
        self
    }

    /// Add a bootnode
    pub fn add_bootnode(mut self, addr: Multiaddr) -> Self {
        self.bootnodes.push(addr);
        self
    }

    /// Set multiple bootnodes
    pub fn bootnodes(mut self, addrs: Vec<Multiaddr>) -> Self {
        self.bootnodes = addrs;
        self
    }

    /// Add a hardfork with a specified condition
    pub fn add_hardfork(mut self, fork: SwarmHardfork, condition: ForkCondition) -> Self {
        self.hardforks.insert(fork, condition);
        self
    }

    /// Add the Accord hardfork at a specific timestamp
    pub fn with_accord(mut self, timestamp: u64) -> Self {
        self.hardforks
            .insert(SwarmHardfork::Accord, ForkCondition::Timestamp(timestamp));
        self
    }

    /// Set the genesis timestamp
    pub fn genesis_timestamp(mut self, timestamp: u64) -> Self {
        self.genesis_timestamp = Some(timestamp);
        self
    }

    /// Set the Swarm token
    pub fn token(mut self, token: Token) -> Self {
        self.token = Some(token);
        self
    }

    /// Build the specification
    pub fn build(self) -> Hive {
        let chain = self.chain.unwrap_or(Chain::from(NamedChain::Dev));
        let network_id = self.network_id.unwrap_or_else(generate_dev_network_id);

        // Use chain name as network name if not specified
        let network_name = self.network_name.unwrap_or_else(|| match chain.named() {
            Some(named) => named.to_string(),
            None => format!("chain-{}", chain.id()),
        });

        // Determine defaults based on chain
        let (token, default_genesis_timestamp) = if chain == Chain::from(NamedChain::Gnosis) {
            (mainnet::TOKEN, SwarmHardfork::MAINNET_GENESIS_TIMESTAMP)
        } else if chain == Chain::from(NamedChain::Sepolia) {
            (testnet::TOKEN, SwarmHardfork::TESTNET_GENESIS_TIMESTAMP)
        } else {
            (dev::TOKEN, 0)
        };

        let token = self.token.unwrap_or(token);
        let genesis_timestamp = self.genesis_timestamp.unwrap_or(default_genesis_timestamp);

        // Ensure we have the Accord hardfork if no hardforks are specified
        let mut hardforks = self.hardforks;
        if hardforks.is_empty() {
            hardforks.insert(
                SwarmHardfork::Accord,
                ForkCondition::Timestamp(genesis_timestamp),
            );
        }

        Hive {
            chain,
            network_id,
            network_name,
            bootnodes: self.bootnodes,
            hardforks,
            token,
            genesis_timestamp,
        }
    }

    /// Create a builder initialized with mainnet settings
    pub fn mainnet() -> Self {
        let spec = init_mainnet();
        Self {
            chain: Some(spec.chain),
            network_id: Some(spec.network_id),
            network_name: Some(spec.network_name.clone()),
            bootnodes: spec.bootnodes.clone(),
            hardforks: spec.hardforks.clone(),
            token: Some(spec.token.clone()),
            genesis_timestamp: Some(spec.genesis_timestamp),
        }
    }

    /// Create a builder initialized with testnet settings
    pub fn testnet() -> Self {
        let spec = init_testnet();
        Self {
            chain: Some(spec.chain),
            network_id: Some(spec.network_id),
            network_name: Some(spec.network_name.clone()),
            bootnodes: spec.bootnodes.clone(),
            hardforks: spec.hardforks.clone(),
            token: Some(spec.token.clone()),
            genesis_timestamp: Some(spec.genesis_timestamp),
        }
    }

    /// Create a builder initialized with development network settings
    pub fn dev() -> Self {
        let spec = init_dev();
        Self {
            chain: Some(spec.chain),
            network_id: Some(spec.network_id),
            network_name: Some(spec.network_name.clone()),
            bootnodes: spec.bootnodes.clone(),
            hardforks: spec.hardforks.clone(),
            token: Some(spec.token.clone()),
            genesis_timestamp: Some(spec.genesis_timestamp),
        }
    }
}

/// Implementation of mainnet bootnodes
fn mainnet_bootnodes() -> Vec<Multiaddr> {
    vec![
        "/ip4/3.127.247.93/tcp/1634/p2p/16Uiu2HAkw5SNNtSvH1zJiQ6Gc3WoGNSxiyNueRKe6fuAuh57G3Bk"
            .parse()
            .unwrap(),
        "/ip4/18.193.69.215/tcp/1634/p2p/16Uiu2HAkzcmk8MeQFnSgA7SGktjR9xCyCyx1rBbGf6rBD6vy5gEi"
            .parse()
            .unwrap(),
        "/ip4/13.51.120.148/tcp/1634/p2p/16Uiu2HAmRGYzi8Huuh4TkUfmVWhVHJ6zzc7e7nFDSQJJoE1nd4Kp"
            .parse()
            .unwrap(),
    ]
}

/// Implementation of testnet bootnodes
fn testnet_bootnodes() -> Vec<Multiaddr> {
    vec![
        "/ip4/3.8.176.112/tcp/1634/p2p/16Uiu2HAkwfcKCxGChwwJN7RyUJ1N85eHN7HyMnP3GJrqKPEUoDfL"
            .parse()
            .unwrap(),
        "/ip4/3.8.176.46/tcp/1634/p2p/16Uiu2HAkzFm9WBXWYnpAKRcZK1HRu1Gv74zW5aw1XzYFz1MGpkqs"
            .parse()
            .unwrap(),
    ]
}

impl Hive {
    /// Returns the genesis timestamp of the network.
    ///
    /// This is the reference point for hardfork activation timing.
    pub fn genesis_timestamp(&self) -> u64 {
        self.genesis_timestamp
    }
}

impl SwarmHardforksTrait for Hive {
    fn swarm_fork_activation(&self, fork: SwarmHardfork) -> ForkCondition {
        self.hardforks.fork(fork)
    }
}

#[cfg(test)]
mod tests {
    use crate::SwarmSpec;

    use super::*;
    use libp2p::multiaddr::Protocol;
    use vertex_net_primitives::Swarm;

    #[test]
    fn test_mainnet_spec() {
        let spec = init_mainnet();
        assert_eq!(spec.network_id, mainnet::NETWORK_ID);
        assert_eq!(spec.network_name, mainnet::NETWORK_NAME);
        assert_eq!(spec.chain, Chain::from(NamedChain::Gnosis));
        assert_eq!(spec.token, mainnet::TOKEN);
    }

    #[test]
    fn test_testnet_spec() {
        let spec = init_testnet();
        assert_eq!(spec.network_id, testnet::NETWORK_ID);
        assert_eq!(spec.network_name, testnet::NETWORK_NAME);
        assert_eq!(spec.chain, Chain::from(NamedChain::Sepolia));
        assert_eq!(spec.token, testnet::TOKEN);
    }

    #[test]
    fn test_default_spec() {
        let spec = Hive::default();
        assert_eq!(spec.chain, Chain::from(NamedChain::Dev));
        assert_eq!(spec.token, dev::TOKEN);
        assert!(spec.hardforks.get(SwarmHardfork::Accord).is_some());
    }

    #[test]
    fn test_dev_network_id() {
        let id1 = generate_dev_network_id();
        let id2 = generate_dev_network_id();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_builder() {
        let multiaddr = Multiaddr::empty()
            .with(Protocol::Ip4([127, 0, 0, 1].into()))
            .with(Protocol::Tcp(1634));

        let spec = HiveBuilder::new()
            .chain(Chain::from(NamedChain::Dev))
            .network_id(1337)
            .network_name("test")
            .add_bootnode(multiaddr.clone())
            .with_accord(1000) // Use the convenience method for Accord
            .build();

        assert_eq!(spec.chain, Chain::from(NamedChain::Dev));
        assert_eq!(spec.network_id, 1337);
        assert_eq!(spec.network_name, "test");
        assert_eq!(spec.bootnodes.len(), 1);
        assert_eq!(spec.bootnodes[0], multiaddr);

        // Check hardfork
        assert!(spec.is_accord_active_at_timestamp(1000));
        assert!(!spec.is_accord_active_at_timestamp(999));
    }

    #[test]
    fn test_builder_from_networks() {
        // Test mainnet builder
        let mainnet_builder = HiveBuilder::mainnet();
        let mainnet_spec = mainnet_builder.build();
        assert_eq!(mainnet_spec.network_id, mainnet::NETWORK_ID);

        // Test testnet builder
        let testnet_builder = HiveBuilder::testnet();
        let testnet_spec = testnet_builder.build();
        assert_eq!(testnet_spec.network_id, testnet::NETWORK_ID);

        // Test dev builder
        let dev_builder = HiveBuilder::dev();
        let dev_spec = dev_builder.build();
        assert_eq!(dev_spec.chain, Chain::from(NamedChain::Dev));
    }

    #[test]
    fn test_swarm_spec_trait_implementation() {
        let spec = init_mainnet();

        // Test swarm() returns the expected Swarm
        assert_eq!(spec.swarm(), Swarm::from_id(1));

        // Test chain() returns the correct chain
        assert_eq!(spec.chain(), Chain::from(NamedChain::Gnosis));

        // Test is_mainnet() and is_testnet()
        assert!(spec.is_mainnet());
        assert!(!spec.is_testnet());

        // Test fork activation
        let genesis_timestamp = SwarmHardfork::MAINNET_GENESIS_TIMESTAMP;
        assert!(spec.is_fork_active_at_timestamp(SwarmHardfork::Accord, genesis_timestamp));
        assert!(!spec.is_fork_active_at_timestamp(SwarmHardfork::Accord, genesis_timestamp - 1));
    }
}
