//! Constants used throughout the Swarm network specification

use crate::types::{StorageContracts, Token};
use vertex_primitives::Address;

/// Mainnet constants
pub mod mainnet {
    use super::*;

    /// Swarm mainnet network ID
    pub const NETWORK_ID: u64 = 1;

    /// Swarm mainnet network name
    pub const NETWORK_NAME: &str = "mainnet";

    /// Swarm token on mainnet (BZZ)
    pub const TOKEN: Token = Token {
        address: Address::new([
            0x2a, 0xc3, 0xc1, 0xd3, 0xe2, 0x4b, 0x45, 0xc6, 0xc3, 0x10, 0x53, 0x4b, 0xc2, 0xdd,
            0x84, 0xb5, 0xed, 0x57, 0x63, 0x35,
        ]),
        name: "Swarm",
        symbol: "BZZ",
        decimals: 16,
    };

    /// Storage contracts for mainnet
    pub const STORAGE_CONTRACTS: StorageContracts = StorageContracts {
        postage: Address::new([
            0x5b, 0x53, 0xf7, 0xa1, 0x97, 0x5e, 0xb2, 0x12, 0xd4, 0xb2, 0x0b, 0x7c, 0xdd, 0x44,
            0x3b, 0xaa, 0x18, 0x9a, 0xf7, 0xc9,
        ]),
        redistribution: Address::new([
            0xeb, 0x21, 0x0c, 0x2e, 0x16, 0x6f, 0x61, 0xb3, 0xfd, 0x32, 0x24, 0x6d, 0x53, 0x89,
            0x3f, 0x8b, 0x9d, 0x2a, 0x62, 0x4c,
        ]),
        staking: Some(Address::new([
            0x0c, 0x6a, 0xa1, 0x97, 0x27, 0x14, 0x66, 0xf0, 0xaf, 0xe3, 0x81, 0x8c, 0xa0, 0x3a,
            0xc4, 0x7d, 0x8f, 0x5c, 0x2f, 0x8a,
        ])),
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
        address: Address::new([
            0x6e, 0x01, 0xee, 0x61, 0x83, 0x72, 0x1a, 0xe9, 0xa0, 0x06, 0xfd, 0x49, 0x06, 0x97,
            0x0c, 0x15, 0x83, 0x86, 0x37, 0x65,
        ]),
        name: "Test Swarm",
        symbol: "tBZZ",
        decimals: 16,
    };

    /// Storage contracts for testnet
    pub const STORAGE_CONTRACTS: StorageContracts = StorageContracts {
        postage: Address::new([
            0x62, 0x1c, 0x2e, 0x0f, 0xa5, 0xed, 0x48, 0x8c, 0x71, 0x24, 0xeb, 0x55, 0xcc, 0x7e,
            0xb3, 0xaf, 0x75, 0xd0, 0xd9, 0xe8,
        ]),
        redistribution: Address::new([
            0xfb, 0x6c, 0x7d, 0x33, 0xbe, 0x1f, 0xb1, 0x2f, 0x4c, 0x5d, 0xa7, 0x1d, 0xf7, 0xc9,
            0xd5, 0xc2, 0x29, 0x70, 0xba, 0x7a,
        ]),
        staking: Some(Address::new([
            0x6f, 0x25, 0x2d, 0xd6, 0xf3, 0x40, 0xf6, 0xc6, 0xd2, 0xf6, 0xee, 0x89, 0x54, 0xb0,
            0x11, 0xdd, 0x5a, 0xba, 0x43, 0x50,
        ])),
    };
}

/// Default values for development networks
pub mod dev {
    use super::*;

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
