//! Constants used throughout the Swarm network specification
//!
//! This module re-exports contract bindings and addresses from `nectar-contracts`
//! and defines network-specific constants like network IDs and names.
//!
//! For storage contract addresses, access them directly from `nectar_contracts`:
//! - `nectar_contracts::mainnet::POSTAGE_STAMP`
//! - `nectar_contracts::mainnet::REDISTRIBUTION`
//! - `nectar_contracts::mainnet::STAKING`
//! - etc.

use crate::Token;

// Re-export contract bindings and deployments from nectar-contracts for convenience.
// This allows consumers to access all contract addresses without depending on nectar-contracts directly.
pub use nectar_contracts::{mainnet as mainnet_contracts, testnet as testnet_contracts};

/// Mainnet constants
pub mod mainnet {
    use super::*;

    /// Swarm mainnet network ID
    pub const NETWORK_ID: u64 = 1;

    /// Swarm mainnet network name
    pub const NETWORK_NAME: &str = "mainnet";

    /// Swarm token on mainnet (xBZZ on Gnosis Chain)
    pub const TOKEN: Token = Token {
        address: nectar_contracts::mainnet::BZZ_TOKEN.address,
        name: "Swarm",
        symbol: "xBZZ",
        decimals: 16,
    };

    /// Swap contracts for mainnet
    pub mod swap {
        /// Chequebook factory address
        pub const CHEQUEBOOK_FACTORY: alloy_primitives::Address =
            nectar_contracts::mainnet::CHEQUEBOOK_FACTORY.address;

        /// Swap price oracle address
        pub const PRICE_ORACLE: alloy_primitives::Address =
            nectar_contracts::mainnet::SWAP_PRICE_ORACLE.address;
    }

    /// Storage contracts re-exported for convenience
    pub mod storage {
        pub use nectar_contracts::mainnet::{
            POSTAGE_STAMP, REDISTRIBUTION, STAKING, STORAGE_PRICE_ORACLE,
        };
    }
}

/// Testnet (Sepolia) constants
pub mod testnet {
    use super::*;

    /// Swarm testnet network ID
    pub const NETWORK_ID: u64 = 10;

    /// Swarm testnet network name
    pub const NETWORK_NAME: &str = "testnet";

    /// Swarm token on testnet (sBZZ on Sepolia)
    pub const TOKEN: Token = Token {
        address: nectar_contracts::testnet::BZZ_TOKEN.address,
        name: "Test Swarm",
        symbol: "sBZZ",
        decimals: 16,
    };

    /// Swap contracts for testnet
    pub mod swap {
        /// Chequebook factory address
        pub const CHEQUEBOOK_FACTORY: alloy_primitives::Address =
            nectar_contracts::testnet::CHEQUEBOOK_FACTORY.address;

        /// Swap price oracle address
        pub const PRICE_ORACLE: alloy_primitives::Address =
            nectar_contracts::testnet::SWAP_PRICE_ORACLE.address;
    }

    /// Storage contracts re-exported for convenience
    pub mod storage {
        pub use nectar_contracts::testnet::{
            POSTAGE_STAMP, REDISTRIBUTION, STAKING, STORAGE_PRICE_ORACLE,
        };
    }
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
}
