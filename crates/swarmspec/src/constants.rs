//! Constants used throughout the Swarm network specification

use crate::{StorageContracts, Token};
use alloy_primitives::address;

/// Mainnet constants
pub mod mainnet {
    use super::*;

    /// Swarm mainnet network ID
    pub const NETWORK_ID: u64 = 1;

    /// Swarm mainnet network name
    pub const NETWORK_NAME: &str = "mainnet";

    /// Swarm token on mainnet (BZZ)
    pub const TOKEN: Token = Token {
        address: address!("2ac3c1d3e24b45c6c310534bc2dd84b5ed576335"),
        name: "Swarm",
        symbol: "BZZ",
        decimals: 16,
    };

    /// Storage contracts for mainnet
    pub const STORAGE_CONTRACTS: StorageContracts = StorageContracts {
        postage: address!("5b53f7a1975eb212d4b20b7cdd443baa189af7c9"),
        redistribution: address!("eb210c2e166f61b3fd32246d53893f8b9d2a624c"),
        staking: Some(address!("0c6aa197271466f0afe3818ca03ac47d8f5c2f8a")),
    };
}

/// Testnet (Sepolia) constants
pub mod testnet {
    use super::*;

    /// Swarm testnet network ID
    pub const NETWORK_ID: u64 = 10;

    /// Swarm testnet network name
    pub const NETWORK_NAME: &str = "testnet";

    /// Swarm token on testnet (tBZZ)
    pub const TOKEN: Token = Token {
        address: address!("6e01ee6183721ae9a006fd4906970c1583863765"),
        name: "Test Swarm",
        symbol: "tBZZ",
        decimals: 16,
    };

    /// Storage contracts for testnet
    pub const STORAGE_CONTRACTS: StorageContracts = StorageContracts {
        postage: address!("621c2e0fa5ed488c7124eb55cc7eb3af75d0d9e8"),
        redistribution: address!("fb6c7d33be1fb12f4c5da71df7c9d5c22970ba7a"),
        staking: Some(address!("6f252dd6f340f6c6d2f6ee8954b011dd5aba4350")),
    };
}

/// Default values for development networks
pub mod dev {
    use super::*;
    use alloy_primitives::Address;

    /// Default network name for development
    pub const NETWORK_NAME: &str = "dev";

    /// Default Swarm token for development
    pub const TOKEN: Token = Token {
        address: Address::ZERO,
        name: "Dev Swarm",
        symbol: "dBZZ",
        decimals: 16,
    };

    /// Default storage contracts for development
    pub const STORAGE_CONTRACTS: StorageContracts = StorageContracts {
        postage: Address::ZERO,
        redistribution: Address::ZERO,
        staking: Some(Address::ZERO),
    };
}
