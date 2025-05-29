//! Swarm network specification implementation

use crate::{
    constants::{dev, mainnet, testnet},
    forks::{ForkCondition, SwarmHardfork, SwarmHardforks},
    generate_dev_network_id,
    types::{PseudosettleConfig, Storage, StorageContracts, Token},
    SwarmSpec,
};

use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use core::fmt::Debug;
use libp2p::Multiaddr;
use vertex_primitives::{hash::keccak256, network::Swarm, B256};

/// A specification for a Swarm network
#[derive(Debug, Clone, PartialEq)]
pub struct NetworkSpec {
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

    /// Pseudosettle configuration for bandwidth
    pub pseudosettle: PseudosettleConfig,

    /// Swarm token details
    pub token: Token,

    /// Genesis hash (derived from network ID)
    pub genesis_hash: B256,
}

impl Default for NetworkSpec {
    fn default() -> Self {
        let mut hardforks = SwarmHardforks::new();
        hardforks.insert(SwarmHardfork::Frontier, ForkCondition::Timestamp(0));

        let network_id = generate_dev_network_id();
        let genesis_hash = generate_genesis_hash(network_id);

        Self {
            network_id,
            network_name: dev::NETWORK_NAME.to_string(),
            bootnodes: Vec::new(),
            hardforks,
            storage: Storage::default(),
            pseudosettle: PseudosettleConfig::default(),
            token: dev::TOKEN,
            genesis_hash,
        }
    }
}

impl SwarmSpec for NetworkSpec {
    fn swarm(&self) -> Swarm {
        Swarm::from_id(self.network_id)
    }

    fn network_id(&self) -> u64 {
        self.network_id
    }

    fn network_name(&self) -> &str {
        &self.network_name
    }

    fn bootnodes(&self) -> &[Multiaddr] {
        &self.bootnodes
    }

    fn is_fork_active_at_timestamp(&self, fork: SwarmHardfork, timestamp: u64) -> bool {
        self.hardforks.is_active_at_timestamp(fork, timestamp)
    }
}

/// Generate a pseudo-random genesis hash from network ID
///
/// This function creates a deterministic hash based on the network ID
/// that can be used as a genesis hash.
fn generate_genesis_hash(network_id: u64) -> B256 {
    let mut bytes = [0u8; 32];
    // Use network ID as seed
    let network_bytes = network_id.to_be_bytes();

    // Fill with a deterministic pattern
    for i in 0..32 {
        bytes[i] = network_bytes[i % network_bytes.len()] ^ (i as u8);
    }

    // Apply keccak256 for better distribution
    B256::from(keccak256(&bytes))
}

/// Create a mainnet network specification
pub fn mainnet_spec() -> NetworkSpec {
    let mut hardforks = SwarmHardforks::new();
    hardforks.insert(
        SwarmHardfork::Frontier,
        ForkCondition::Timestamp(SwarmHardfork::MAINNET_GENESIS_TIMESTAMP),
    );

    let genesis_hash = generate_genesis_hash(mainnet::NETWORK_ID);

    NetworkSpec {
        network_id: mainnet::NETWORK_ID,
        network_name: mainnet::NETWORK_NAME.to_string(),
        bootnodes: mainnet_bootnodes(),
        hardforks,
        storage: Storage {
            contracts: mainnet::STORAGE_CONTRACTS,
            ..Default::default()
        },
        pseudosettle: PseudosettleConfig::default(),
        token: mainnet::TOKEN,
        genesis_hash,
    }
}

/// Create a testnet network specification
pub fn testnet_spec() -> NetworkSpec {
    let mut hardforks = SwarmHardforks::new();
    hardforks.insert(
        SwarmHardfork::Frontier,
        ForkCondition::Timestamp(SwarmHardfork::TESTNET_GENESIS_TIMESTAMP),
    );

    let genesis_hash = generate_genesis_hash(testnet::NETWORK_ID);

    NetworkSpec {
        network_id: testnet::NETWORK_ID,
        network_name: testnet::NETWORK_NAME.to_string(),
        bootnodes: testnet_bootnodes(),
        hardforks,
        storage: Storage {
            contracts: testnet::STORAGE_CONTRACTS,
            ..Default::default()
        },
        pseudosettle: PseudosettleConfig::default(),
        token: testnet::TOKEN,
        genesis_hash,
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

/// Lazy-loaded mainnet specification
pub fn mainnet() -> &'static NetworkSpec {
    use once_cell::sync::Lazy;
    static INSTANCE: Lazy<NetworkSpec> = Lazy::new(mainnet_spec);
    &INSTANCE
}

/// Lazy-loaded testnet specification
pub fn testnet() -> &'static NetworkSpec {
    use once_cell::sync::Lazy;
    static INSTANCE: Lazy<NetworkSpec> = Lazy::new(testnet_spec);
    &INSTANCE
}
