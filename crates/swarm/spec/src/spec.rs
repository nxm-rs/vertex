//! Concrete network specifications
//!
//! This module provides [`Spec`], the standard implementation of [`SwarmSpec`]
//! used for mainnet, testnet, development, and custom networks.
//!
//! Pre-built specifications are available via [`crate::init_mainnet`], [`crate::init_testnet`],
//! and [`crate::init_dev`] (requires `std` feature). Custom specifications can be
//! constructed with [`SpecBuilder`].

#[cfg(feature = "std")]
use crate::error::SwarmSpecFileError;
use crate::{
    Token,
    constants::{DEFAULT_RESERVE_CAPACITY, dev, mainnet, testnet},
    generate_dev_network_id,
};
#[cfg(feature = "std")]
use alloc::sync::Arc;
#[cfg(feature = "std")]
use alloc::vec;
use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};
use alloy_chains::{Chain, NamedChain};
#[cfg(feature = "std")]
use std::sync::OnceLock;
use vertex_swarm_forks::{ForkCondition, SwarmHardfork, SwarmHardforks, SwarmHardforksTrait};

/// A concrete Swarm network specification.
///
/// `Spec` captures everything needed to identify and connect to a Swarm network:
/// which blockchain it settles on, how to find peers, when protocol upgrades
/// activate, and which token contract to use.
///
/// # Usage
///
/// For standard networks, use the provided initializers:
/// - [`crate::init_mainnet()`] - Production network on Gnosis Chain
/// - [`crate::init_testnet()`] - Test network on Sepolia
/// - [`crate::init_dev()`] - Local development with auto-generated network ID
///
/// For custom networks, use [`SpecBuilder`] or load from a TOML file with `TryFrom<&Path>`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Spec {
    /// Underlying blockchain
    #[serde(default = "default_chain")]
    pub chain: Chain,

    /// Network ID for this Swarm network
    pub network_id: u64,

    /// Network name (e.g., "mainnet", "testnet")
    #[serde(default)]
    pub network_name: String,

    /// Bootnodes - entry points into the network (as multiaddr strings).
    ///
    /// Consumers should parse these as `Multiaddr` in the networking layer.
    #[serde(default)]
    pub bootnodes: Vec<String>,

    /// Hardforks configuration (not serialized - uses default with Genesis at timestamp 0)
    #[serde(skip, default = "default_hardforks")]
    pub hardforks: SwarmHardforks,

    /// Swarm token details (not serialized - uses dev token defaults)
    #[serde(skip, default = "default_token")]
    pub token: Token,

    /// Genesis timestamp (reference point for hardfork activation)
    #[serde(default)]
    pub genesis_timestamp: u64,

    /// Reserve capacity in number of chunks for full nodes (typically 2^22)
    #[serde(default = "default_reserve_capacity")]
    pub reserve_capacity: u64,
}

fn default_chain() -> Chain {
    Chain::from(NamedChain::Dev)
}

fn default_hardforks() -> SwarmHardforks {
    SwarmHardfork::dev().into()
}

fn default_token() -> Token {
    dev::TOKEN
}

fn default_reserve_capacity() -> u64 {
    DEFAULT_RESERVE_CAPACITY
}

impl Default for Spec {
    fn default() -> Self {
        Self {
            chain: Chain::from(NamedChain::Dev),
            network_id: generate_dev_network_id(),
            network_name: dev::NETWORK_NAME.to_string(),
            bootnodes: Vec::new(),
            hardforks: SwarmHardfork::dev().into(),
            token: dev::TOKEN,
            genesis_timestamp: 0,
            reserve_capacity: DEFAULT_RESERVE_CAPACITY,
        }
    }
}

/// The Swarm mainnet specification
#[cfg(feature = "std")]
pub static MAINNET: OnceLock<Arc<Spec>> = OnceLock::new();

/// Initialize the mainnet specification
#[cfg(feature = "std")]
pub(crate) fn init_mainnet() -> Arc<Spec> {
    MAINNET
        .get_or_init(|| {
            let spec = Spec {
                chain: Chain::from(NamedChain::Gnosis),
                network_id: mainnet::NETWORK_ID,
                network_name: mainnet::NETWORK_NAME.to_string(),
                bootnodes: mainnet_bootnodes(),
                hardforks: SwarmHardfork::mainnet().into(),
                token: mainnet::TOKEN,
                genesis_timestamp: SwarmHardfork::MAINNET_GENESIS_TIMESTAMP,
                reserve_capacity: DEFAULT_RESERVE_CAPACITY,
            };

            Arc::new(spec)
        })
        .clone()
}

/// The Swarm testnet specification
#[cfg(feature = "std")]
pub static TESTNET: OnceLock<Arc<Spec>> = OnceLock::new();

/// Initialize the testnet specification
#[cfg(feature = "std")]
pub(crate) fn init_testnet() -> Arc<Spec> {
    TESTNET
        .get_or_init(|| {
            let spec = Spec {
                chain: Chain::from(NamedChain::Sepolia),
                network_id: testnet::NETWORK_ID,
                network_name: testnet::NETWORK_NAME.to_string(),
                bootnodes: testnet_bootnodes(),
                hardforks: SwarmHardfork::testnet().into(),
                token: testnet::TOKEN,
                genesis_timestamp: SwarmHardfork::TESTNET_GENESIS_TIMESTAMP,
                reserve_capacity: DEFAULT_RESERVE_CAPACITY,
            };

            Arc::new(spec)
        })
        .clone()
}

/// The Swarm development network specification
#[cfg(feature = "std")]
pub static DEV: OnceLock<Arc<Spec>> = OnceLock::new();

/// Initialize the dev specification
#[cfg(feature = "std")]
pub(crate) fn init_dev() -> Arc<Spec> {
    DEV.get_or_init(|| Arc::new(Spec::default())).clone()
}

/// Builder for constructing custom [`Spec`] specifications.
///
/// Start from scratch with [`SpecBuilder::new()`], or derive from an existing
/// network with [`SpecBuilder::mainnet()`], [`SpecBuilder::testnet()`], or
/// [`SpecBuilder::dev()`] and override specific fields.
#[derive(Default, Clone)]
pub struct SpecBuilder {
    chain: Option<Chain>,
    network_id: Option<u64>,
    network_name: Option<String>,
    bootnodes: Vec<String>,
    hardforks: SwarmHardforks,
    token: Option<Token>,
    genesis_timestamp: Option<u64>,
    reserve_capacity: Option<u64>,
}

impl SpecBuilder {
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

    /// Add a bootnode (as a multiaddr string).
    pub fn add_bootnode(mut self, addr: impl ToString) -> Self {
        self.bootnodes.push(addr.to_string());
        self
    }

    /// Set multiple bootnodes (as multiaddr strings).
    pub fn bootnodes(mut self, addrs: Vec<String>) -> Self {
        self.bootnodes = addrs;
        self
    }

    /// Add a hardfork with a specified condition
    pub fn add_hardfork(mut self, fork: SwarmHardfork, condition: ForkCondition) -> Self {
        self.hardforks.insert(fork, condition);
        self
    }

    /// Add the Genesis hardfork at a specific timestamp
    pub fn with_genesis(mut self, timestamp: u64) -> Self {
        self.hardforks
            .insert(SwarmHardfork::Genesis, ForkCondition::Timestamp(timestamp));
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

    /// Set the reserve capacity in number of chunks
    pub fn reserve_capacity(mut self, capacity: u64) -> Self {
        self.reserve_capacity = Some(capacity);
        self
    }

    /// Build the specification
    pub fn build(self) -> Spec {
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

        // Ensure we have the Genesis hardfork if no hardforks are specified
        let mut hardforks = self.hardforks;
        if hardforks.is_empty() {
            hardforks.insert(
                SwarmHardfork::Genesis,
                ForkCondition::Timestamp(genesis_timestamp),
            );
        }

        Spec {
            chain,
            network_id,
            network_name,
            bootnodes: self.bootnodes,
            hardforks,
            token,
            genesis_timestamp,
            reserve_capacity: self.reserve_capacity.unwrap_or(DEFAULT_RESERVE_CAPACITY),
        }
    }

    /// Create a builder initialized with mainnet settings
    #[cfg(feature = "std")]
    pub fn mainnet() -> Self {
        Self::from(init_mainnet().as_ref())
    }

    /// Create a builder initialized with testnet settings
    #[cfg(feature = "std")]
    pub fn testnet() -> Self {
        Self::from(init_testnet().as_ref())
    }

    /// Create a builder initialized with development network settings
    #[cfg(feature = "std")]
    pub fn dev() -> Self {
        Self::from(init_dev().as_ref())
    }
}

impl From<&Spec> for SpecBuilder {
    fn from(spec: &Spec) -> Self {
        Self {
            chain: Some(spec.chain),
            network_id: Some(spec.network_id),
            network_name: Some(spec.network_name.clone()),
            bootnodes: spec.bootnodes.clone(),
            hardforks: spec.hardforks.clone(),
            token: Some(spec.token.clone()),
            genesis_timestamp: Some(spec.genesis_timestamp),
            reserve_capacity: Some(spec.reserve_capacity),
        }
    }
}

/// Mainnet bootnodes using dnsaddr for dynamic resolution.
///
/// The `/dnsaddr/mainnet.ethswarm.org` multiaddr is resolved at runtime via DNS TXT
/// records, allowing the Swarm team to update bootnode IPs without client changes.
/// Resolution should happen in the networking layer.
#[cfg(feature = "std")]
fn mainnet_bootnodes() -> Vec<String> {
    vec!["/dnsaddr/mainnet.ethswarm.org".to_string()]
}

/// Testnet bootnodes using dnsaddr for dynamic resolution.
///
/// The `/dnsaddr/testnet.ethswarm.org` multiaddr is resolved at runtime via DNS TXT
/// records. Resolution should happen in the networking layer.
#[cfg(feature = "std")]
fn testnet_bootnodes() -> Vec<String> {
    vec!["/dnsaddr/testnet.ethswarm.org".to_string()]
}

impl Spec {
    /// Returns the genesis timestamp of the network.
    ///
    /// This is the reference point for hardfork activation timing.
    #[must_use]
    pub fn genesis_timestamp(&self) -> u64 {
        self.genesis_timestamp
    }

    /// Serialize this SwarmSpec to a TOML string.
    #[cfg(feature = "std")]
    pub fn to_toml(&self) -> Result<String, SwarmSpecFileError> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Write this SwarmSpec to a TOML file.
    #[cfg(feature = "std")]
    pub fn to_file(&self, path: &std::path::Path) -> Result<(), SwarmSpecFileError> {
        let toml = self.to_toml()?;
        std::fs::write(path, toml)?;
        Ok(())
    }
}

/// Parse a [`Spec`] from a TOML string.
#[cfg(feature = "std")]
impl TryFrom<&str> for Spec {
    type Error = SwarmSpecFileError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        Ok(toml::from_str(s)?)
    }
}

/// Load a [`Spec`] from a TOML file path.
///
/// ```toml
/// network_id = 0
/// network_name = "local-kurtosis"
/// bootnodes = ["/ip4/127.0.0.1/tcp/1634/p2p/QmXxx..."]
/// genesis_timestamp = 0
/// reserve_capacity = 4194304
/// ```
#[cfg(feature = "std")]
impl TryFrom<&std::path::Path> for Spec {
    type Error = SwarmSpecFileError;

    fn try_from(path: &std::path::Path) -> Result<Self, Self::Error> {
        let content = std::fs::read_to_string(path)?;
        Self::try_from(content.as_str())
    }
}

impl SwarmHardforksTrait for Spec {
    fn swarm_fork_activation(&self, fork: SwarmHardfork) -> ForkCondition {
        self.hardforks.fork(fork)
    }
}

#[cfg(test)]
mod tests {
    use crate::SwarmSpec;

    use super::*;
    use nectar_swarms::Swarm;

    #[test]
    fn test_mainnet_spec() {
        let spec = init_mainnet();
        assert_eq!(spec.network_id, mainnet::NETWORK_ID);
        assert_eq!(spec.network_name, mainnet::NETWORK_NAME);
        assert_eq!(spec.chain, Chain::from(NamedChain::Gnosis));
        assert!(spec.token == mainnet::TOKEN);
    }

    #[test]
    fn test_testnet_spec() {
        let spec = init_testnet();
        assert_eq!(spec.network_id, testnet::NETWORK_ID);
        assert_eq!(spec.network_name, testnet::NETWORK_NAME);
        assert_eq!(spec.chain, Chain::from(NamedChain::Sepolia));
        assert!(spec.token == testnet::TOKEN);
    }

    #[test]
    fn test_default_spec() {
        let spec = Spec::default();
        assert_eq!(spec.chain, Chain::from(NamedChain::Dev));
        assert!(spec.token == dev::TOKEN);
        // Dev network has both Genesis and Accord hardforks
        assert!(spec.hardforks.get(SwarmHardfork::Genesis).is_some());
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
        let bootnode = "/ip4/127.0.0.1/tcp/1634";

        let spec = SpecBuilder::new()
            .chain(Chain::from(NamedChain::Dev))
            .network_id(1337)
            .network_name("test")
            .add_bootnode(bootnode)
            .with_accord(1000)
            .build();

        assert_eq!(spec.chain, Chain::from(NamedChain::Dev));
        assert_eq!(spec.network_id, 1337);
        assert_eq!(spec.network_name, "test");
        assert_eq!(spec.bootnodes.len(), 1);
        assert_eq!(spec.bootnodes[0], bootnode);

        // Check hardfork
        assert!(spec.is_accord_active_at_timestamp(1000));
        assert!(!spec.is_accord_active_at_timestamp(999));
    }

    #[test]
    fn test_builder_from_networks() {
        // Test mainnet builder
        let mainnet_builder = SpecBuilder::mainnet();
        let mainnet_spec = mainnet_builder.build();
        assert_eq!(mainnet_spec.network_id, mainnet::NETWORK_ID);

        // Test testnet builder
        let testnet_builder = SpecBuilder::testnet();
        let testnet_spec = testnet_builder.build();
        assert_eq!(testnet_spec.network_id, testnet::NETWORK_ID);

        // Test dev builder
        let dev_builder = SpecBuilder::dev();
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
        assert!(spec.is_fork_active_at_timestamp(SwarmHardfork::Genesis, genesis_timestamp));
        assert!(!spec.is_fork_active_at_timestamp(SwarmHardfork::Genesis, genesis_timestamp - 1));
    }
}
