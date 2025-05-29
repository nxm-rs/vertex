//! Network specification for Swarm networks

use crate::{
    constants::{dev, mainnet, testnet},
    generate_dev_network_id, LightClient, Storage, StorageContracts, Token,
};
use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use alloy_chains::{Chain, NamedChain};
use alloy_eip2124::{ForkFilter, ForkFilterKey, ForkHash, ForkId, Head};
use alloy_primitives::B256;
use libp2p::Multiaddr;
use vertex_network_primitives_traits::OnceLock;
use vertex_swarm_forks::{
    ForkCondition, Hardfork, Hardforks, SwarmHardfork, SwarmHardforks, SwarmHardforksTrait,
};

/// A specification for a Swarm network
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkSpec {
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

    /// Storage configuration
    pub storage: Storage,

    /// Bandwidth incentives configuration
    pub light_client: LightClient,

    /// Swarm token details
    pub token: Token,

    /// Genesis hash (derived from network ID)
    pub genesis_hash: B256,

    /// Genesis timestamp
    pub genesis_timestamp: u64,
}

impl Default for NetworkSpec {
    fn default() -> Self {
        let mut hardforks = SwarmHardforks::new(vec![]);
        hardforks.insert(SwarmHardfork::Frontier, ForkCondition::Timestamp(0));

        let genesis_hash = generate_genesis_hash(generate_dev_network_id());

        Self {
            chain: Chain::from(NamedChain::Dev),
            network_id: generate_dev_network_id(),
            network_name: dev::NETWORK_NAME.to_string(),
            bootnodes: Vec::new(),
            hardforks,
            storage: Storage {
                contracts: dev::STORAGE_CONTRACTS,
                ..Default::default()
            },
            light_client: LightClient::default(),
            token: dev::TOKEN,
            genesis_hash,
            genesis_timestamp: 0,
        }
    }
}

/// The Swarm mainnet specification
pub static MAINNET: OnceLock<Arc<NetworkSpec>> = OnceLock::new();

/// Initialize the mainnet specification
pub fn init_mainnet() -> Arc<NetworkSpec> {
    MAINNET
        .get_or_init(|| {
            let mut hardforks = SwarmHardforks::new(vec![]);
            hardforks.insert(
                SwarmHardfork::Frontier,
                ForkCondition::Timestamp(SwarmHardfork::MAINNET_GENESIS_TIMESTAMP),
            );

            let genesis_hash = generate_genesis_hash(mainnet::NETWORK_ID);

            let spec = NetworkSpec {
                chain: Chain::from(NamedChain::Gnosis),
                network_id: mainnet::NETWORK_ID,
                network_name: mainnet::NETWORK_NAME.to_string(),
                bootnodes: mainnet_bootnodes(),
                hardforks,
                storage: Storage {
                    contracts: mainnet::STORAGE_CONTRACTS,
                    ..Default::default()
                },
                light_client: LightClient::default(),
                token: mainnet::TOKEN,
                genesis_hash,
                genesis_timestamp: SwarmHardfork::MAINNET_GENESIS_TIMESTAMP,
            };

            Arc::new(spec)
        })
        .clone()
}

/// The Swarm testnet specification
pub static TESTNET: OnceLock<Arc<NetworkSpec>> = OnceLock::new();

/// Initialize the testnet specification
pub fn init_testnet() -> Arc<NetworkSpec> {
    TESTNET
        .get_or_init(|| {
            let mut hardforks = SwarmHardforks::new(vec![]);
            hardforks.insert(
                SwarmHardfork::Frontier,
                ForkCondition::Timestamp(SwarmHardfork::TESTNET_GENESIS_TIMESTAMP),
            );

            let genesis_hash = generate_genesis_hash(testnet::NETWORK_ID);

            let spec = NetworkSpec {
                chain: Chain::from(NamedChain::Sepolia),
                network_id: testnet::NETWORK_ID,
                network_name: testnet::NETWORK_NAME.to_string(),
                bootnodes: testnet_bootnodes(),
                hardforks,
                storage: Storage {
                    contracts: testnet::STORAGE_CONTRACTS,
                    ..Default::default()
                },
                light_client: LightClient::default(),
                token: testnet::TOKEN,
                genesis_hash,
                genesis_timestamp: SwarmHardfork::TESTNET_GENESIS_TIMESTAMP,
            };

            Arc::new(spec)
        })
        .clone()
}

/// The Swarm development network specification
pub static DEV: OnceLock<Arc<NetworkSpec>> = OnceLock::new();

/// Initialize the dev specification
pub fn init_dev() -> Arc<NetworkSpec> {
    DEV.get_or_init(|| Arc::new(NetworkSpec::default())).clone()
}

/// An interface for accessing Swarm specifications
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmSpecProvider: Send + Sync {
    /// Get an Arc to the specification
    fn spec(&self) -> Arc<NetworkSpec>;
}

/// Helper struct to build custom Swarm network specifications
#[derive(Debug, Default, Clone)]
pub struct SwarmSpecBuilder {
    chain: Option<Chain>,
    network_id: Option<u64>,
    network_name: Option<String>,
    bootnodes: Vec<Multiaddr>,
    hardforks: SwarmHardforks,
    storage: Option<Storage>,
    bandwidth: Option<LightClient>,
    token: Option<Token>,
    genesis_timestamp: Option<u64>,
}

impl SwarmSpecBuilder {
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

    /// Add the Frontier hardfork at a specific timestamp
    pub fn with_frontier(mut self, timestamp: u64) -> Self {
        self.hardforks
            .insert(SwarmHardfork::Frontier, ForkCondition::Timestamp(timestamp));
        self
    }

    /// Set the genesis timestamp
    pub fn genesis_timestamp(mut self, timestamp: u64) -> Self {
        self.genesis_timestamp = Some(timestamp);
        self
    }

    /// Set the storage configuration
    pub fn storage(mut self, config: Storage) -> Self {
        self.storage = Some(config);
        self
    }

    /// Set the storage contracts
    pub fn storage_contracts(mut self, contracts: StorageContracts) -> Self {
        let storage = self.storage.get_or_insert_with(Storage::default);
        storage.contracts = contracts;
        self
    }

    /// Set the bandwidth incentives configuration
    pub fn bandwidth(mut self, config: LightClient) -> Self {
        self.bandwidth = Some(config);
        self
    }

    /// Set the Swarm token
    pub fn token(mut self, token: Token) -> Self {
        self.token = Some(token);
        self
    }

    /// Build the specification
    pub fn build(self) -> NetworkSpec {
        let chain = self.chain.unwrap_or(Chain::from(NamedChain::Dev));
        let network_id = self.network_id.unwrap_or_else(generate_dev_network_id);

        // Use chain name as network name if not specified
        let network_name = self.network_name.unwrap_or_else(|| match chain.named() {
            Some(named) => named.to_string(),
            None => format!("chain-{}", chain.id()),
        });

        // Determine defaults based on chain
        let (storage_contracts, token, default_genesis_timestamp) =
            if chain == Chain::from(NamedChain::Gnosis) {
                (
                    mainnet::STORAGE_CONTRACTS,
                    mainnet::TOKEN,
                    SwarmHardfork::MAINNET_GENESIS_TIMESTAMP,
                )
            } else if chain == Chain::from(NamedChain::Sepolia) {
                (
                    testnet::STORAGE_CONTRACTS,
                    testnet::TOKEN,
                    SwarmHardfork::TESTNET_GENESIS_TIMESTAMP,
                )
            } else {
                (dev::STORAGE_CONTRACTS, dev::TOKEN, 0)
            };

        // Use provided storage or create with appropriate defaults
        let storage = self.storage.unwrap_or_else(|| {
            let mut default_storage = Storage::default();
            default_storage.contracts = storage_contracts;
            default_storage
        });

        let bandwidth = self.bandwidth.unwrap_or_default();
        let token = self.token.unwrap_or(token);
        let genesis_timestamp = self.genesis_timestamp.unwrap_or(default_genesis_timestamp);

        // Ensure we have the Frontier hardfork if no hardforks are specified
        let mut hardforks = self.hardforks;
        if hardforks.is_empty() {
            hardforks.insert(
                SwarmHardfork::Frontier,
                ForkCondition::Timestamp(genesis_timestamp),
            );
        }

        let genesis_hash = generate_genesis_hash(network_id);

        NetworkSpec {
            chain,
            network_id,
            network_name,
            bootnodes: self.bootnodes,
            hardforks,
            storage,
            light_client: bandwidth,
            token,
            genesis_hash,
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
            storage: Some(spec.storage.clone()),
            bandwidth: Some(spec.light_client.clone()),
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
            storage: Some(spec.storage.clone()),
            bandwidth: Some(spec.light_client.clone()),
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
            storage: Some(spec.storage.clone()),
            bandwidth: Some(spec.light_client.clone()),
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

/// Generate a pseudo-random genesis hash from network ID
///
/// This function creates a deterministic hash based on the network ID
/// that can be used as a genesis hash.
fn generate_genesis_hash(network_id: u64) -> B256 {
    let mut hash = [0u8; 32];
    // Use network ID as seed for a simple deterministic hash
    let network_bytes = network_id.to_be_bytes();

    // Fill the hash with a pattern based on network ID
    for i in 0..32 {
        hash[i] = network_bytes[i % 8] ^ (i as u8);
    }

    hash.into()
}

impl NetworkSpec {
    /// Returns the genesis hash of the chain
    pub fn genesis_hash(&self) -> B256 {
        self.genesis_hash
    }

    /// Returns the genesis timestamp of the chain
    pub fn genesis_timestamp(&self) -> u64 {
        self.genesis_timestamp
    }

    /// Convenience method to get the fork id for [`SwarmHardfork::Frontier`] from the spec.
    #[inline]
    pub fn frontier_fork_id(&self) -> Option<ForkId> {
        self.hardfork_fork_id(SwarmHardfork::Frontier)
    }

    /// Gets the fork id for a specific hardfork from the spec.
    #[inline]
    pub fn hardfork_fork_id<H: Hardfork>(&self, hardfork: H) -> Option<ForkId> {
        let condition = self.hardforks.fork(hardfork);
        match condition {
            ForkCondition::Timestamp(timestamp) => {
                let head = Head {
                    timestamp,
                    number: 0,
                    ..Default::default()
                };
                Some(self.fork_id(&head))
            }
            ForkCondition::Block(block) => {
                let head = Head {
                    number: block,
                    ..Default::default()
                };
                Some(self.fork_id(&head))
            }
            ForkCondition::Never => None,
        }
    }

    /// Convenience method to get the latest fork id from the spec.
    #[inline]
    pub fn latest_fork_id(&self) -> ForkId {
        self.hardfork_fork_id(self.hardforks.last().unwrap().0)
            .unwrap()
    }

    /// Creates a [`ForkFilter`] for the block described by [Head].
    pub fn fork_filter(&self, head: Head) -> ForkFilter {
        let forks = self.hardforks.forks_iter().filter_map(|(_, condition)| {
            // Filter out Never conditions
            match condition {
                ForkCondition::Block(block) => Some(ForkFilterKey::Block(block)),
                ForkCondition::Timestamp(time) => Some(ForkFilterKey::Time(time)),
                ForkCondition::Never => None,
            }
        });

        ForkFilter::new(head, self.genesis_hash(), self.genesis_timestamp(), forks)
    }

    /// Compute the [`ForkId`] for the given [`Head`] following eip-2124 spec.
    ///
    /// Note: In case there are multiple hardforks activated at the same block or timestamp, only
    /// the first gets applied.
    pub fn fork_id(&self, head: &Head) -> ForkId {
        let mut forkhash = ForkHash::from(self.genesis_hash);

        // This tracks the last applied timestamp or block fork
        let mut current_applied = 0;

        // Handle all block forks first
        for (_, cond) in self.hardforks.forks_iter() {
            if let ForkCondition::Block(block) = cond {
                if cond.active_at_head(head) {
                    // Skip duplicated hardforks enabled at the same block
                    if block != current_applied {
                        forkhash += block;
                        current_applied = block;
                    }
                } else {
                    // We can return here because this block fork is not active
                    return ForkId {
                        hash: forkhash,
                        next: block,
                    };
                }
            }
        }

        // Handle all timestamp forks after block forks
        for (_, cond) in self.hardforks.forks_iter() {
            if let ForkCondition::Timestamp(timestamp) = cond {
                if cond.active_at_head(head) {
                    // Skip duplicated hardforks activated at the same timestamp
                    if timestamp != current_applied {
                        forkhash += timestamp;
                        current_applied = timestamp;
                    }
                } else {
                    // This timestamp fork is not active yet
                    return ForkId {
                        hash: forkhash,
                        next: timestamp,
                    };
                }
            }
        }

        // All forks are active
        ForkId {
            hash: forkhash,
            next: 0,
        }
    }

    /// An internal helper function that returns a head block that satisfies a given Fork condition.
    pub(crate) fn satisfy(&self, cond: ForkCondition) -> Head {
        match cond {
            ForkCondition::Block(number) => Head {
                number,
                ..Default::default()
            },
            ForkCondition::Timestamp(timestamp) => Head {
                timestamp,
                ..Default::default()
            },
            ForkCondition::Never => unreachable!(),
        }
    }
}

impl SwarmHardforksTrait for NetworkSpec {
    fn swarm_fork_activation(&self, fork: SwarmHardfork) -> ForkCondition {
        self.hardforks.fork(fork)
    }
}

#[cfg(test)]
mod tests {
    use crate::SwarmSpec;

    use super::*;
    use libp2p::multiaddr::Protocol;
    use vertex_network_primitives::Swarm;

    #[test]
    fn test_mainnet_spec() {
        let spec = init_mainnet();
        assert_eq!(spec.network_id, mainnet::NETWORK_ID);
        assert_eq!(spec.network_name, mainnet::NETWORK_NAME);
        assert_eq!(spec.chain, Chain::from(NamedChain::Gnosis));
        assert_eq!(
            spec.storage.contracts.staking,
            Some(mainnet::STORAGE_CONTRACTS.staking.unwrap())
        );
        assert_eq!(spec.token, mainnet::TOKEN);
    }

    #[test]
    fn test_testnet_spec() {
        let spec = init_testnet();
        assert_eq!(spec.network_id, testnet::NETWORK_ID);
        assert_eq!(spec.network_name, testnet::NETWORK_NAME);
        assert_eq!(spec.chain, Chain::from(NamedChain::Sepolia));
        assert_eq!(
            spec.storage.contracts.staking,
            Some(testnet::STORAGE_CONTRACTS.staking.unwrap())
        );
        assert_eq!(spec.token, testnet::TOKEN);
    }

    #[test]
    fn test_default_spec() {
        let spec = NetworkSpec::default();
        assert_eq!(spec.chain, Chain::from(NamedChain::Dev));
        assert_eq!(spec.storage.contracts, dev::STORAGE_CONTRACTS);
        assert_eq!(spec.token, dev::TOKEN);
        assert!(spec.hardforks.get(SwarmHardfork::Frontier).is_some());
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

        let spec = SwarmSpecBuilder::new()
            .chain(Chain::from(NamedChain::Dev))
            .network_id(1337)
            .network_name("test")
            .add_bootnode(multiaddr.clone())
            .with_frontier(1000) // Use the convenience method for Frontier
            .build();

        assert_eq!(spec.chain, Chain::from(NamedChain::Dev));
        assert_eq!(spec.network_id, 1337);
        assert_eq!(spec.network_name, "test");
        assert_eq!(spec.bootnodes.len(), 1);
        assert_eq!(spec.bootnodes[0], multiaddr);

        // Check hardfork
        assert!(spec.is_frontier_active_at_timestamp(1000));
        assert!(!spec.is_frontier_active_at_timestamp(999));
    }

    #[test]
    fn test_builder_from_networks() {
        // Test mainnet builder
        let mainnet_builder = SwarmSpecBuilder::mainnet();
        let mainnet_spec = mainnet_builder.build();
        assert_eq!(mainnet_spec.network_id, mainnet::NETWORK_ID);

        // Test testnet builder
        let testnet_builder = SwarmSpecBuilder::testnet();
        let testnet_spec = testnet_builder.build();
        assert_eq!(testnet_spec.network_id, testnet::NETWORK_ID);

        // Test dev builder
        let dev_builder = SwarmSpecBuilder::dev();
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
        assert!(spec.is_fork_active_at_timestamp(SwarmHardfork::Frontier, genesis_timestamp));
        assert!(!spec.is_fork_active_at_timestamp(SwarmHardfork::Frontier, genesis_timestamp - 1));
    }

    #[test]
    fn test_fork_id() {
        let spec = init_mainnet();

        // Get fork ID at genesis
        let head_genesis = Head {
            timestamp: SwarmHardfork::MAINNET_GENESIS_TIMESTAMP,
            ..Default::default()
        };
        let genesis_fork_id = spec.fork_id(&head_genesis);

        // Get fork ID at a future head
        let head_future = Head {
            timestamp: SwarmHardfork::MAINNET_GENESIS_TIMESTAMP + 1000,
            ..Default::default()
        };
        let future_fork_id = spec.fork_id(&head_future);

        // Genesis should have been processed in both cases
        assert_eq!(genesis_fork_id.hash, future_fork_id.hash);

        // Next value should be 0 since all forks are active
        assert_eq!(future_fork_id.next, 0);
    }

    #[test]
    fn test_hardfork_fork_id() {
        let spec = init_mainnet();

        // Get frontier fork ID
        let frontier_fork_id = spec.frontier_fork_id().unwrap();

        // This should match the fork ID at the frontier timestamp
        let head = Head {
            timestamp: SwarmHardfork::MAINNET_GENESIS_TIMESTAMP,
            ..Default::default()
        };
        let head_fork_id = spec.fork_id(&head);

        assert_eq!(frontier_fork_id, head_fork_id);
    }
}
