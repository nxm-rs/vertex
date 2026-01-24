//! Constants used throughout the Swarm network specification
//!
//! This module defines protocol-level constants that all nodes must agree on,
//! as well as network-specific constants like network IDs and contract addresses.
//!
//! # Chunk Protocol Constants
//!
//! The fundamental unit of storage in Swarm is the chunk. All chunks have a fixed
//! size determined by the Binary Merkle Tree (BMT) structure:
//!
//! - [`SECTION_SIZE`]: Hash size (32 bytes, Keccak-256)
//! - [`BRANCHES`]: BMT branching factor (128)
//! - [`CHUNK_SIZE`]: `SECTION_SIZE * BRANCHES` = 4096 bytes (2^12)
//!
//! # Storage Capacity Constants
//!
//! - [`DEFAULT_RESERVE_CAPACITY`]: Default number of chunks for a full node (2^22)
//!
//! # Contract Addresses
//!
//! For storage contract addresses, access them directly from `nectar_contracts`:
//! - `nectar_contracts::mainnet::POSTAGE_STAMP`
//! - `nectar_contracts::mainnet::REDISTRIBUTION`
//! - `nectar_contracts::mainnet::STAKING`
//! - etc.

use crate::Token;

/// Default section size in bytes (hash output size).
///
/// This is the size of a Keccak-256 hash output, which forms the basis
/// of the BMT structure.
pub const DEFAULT_SECTION_SIZE: usize = 32;

/// Default BMT branching factor.
///
/// Each internal node in the Binary Merkle Tree has this many children.
/// Combined with section_size, this determines the chunk size.
pub const DEFAULT_BRANCHES: usize = 128;

/// Default chunk size as a power of 2 exponent.
///
/// This is the canonical definition. `DEFAULT_CHUNK_SIZE = 2^DEFAULT_CHUNK_SIZE_LOG2`
pub const DEFAULT_CHUNK_SIZE_LOG2: u32 = 12;

/// Default chunk size in bytes.
///
/// This is the fundamental unit of storage in Swarm. All chunks (both CAC
/// and SOC) have this maximum payload size.
///
/// Derived from: 2^12 = 4096 bytes
/// Equivalently: `SECTION_SIZE * BRANCHES` = 32 * 128 = 4096 bytes
pub const DEFAULT_CHUNK_SIZE: usize = 1 << DEFAULT_CHUNK_SIZE_LOG2;

/// Default reserve capacity as a power of 2 exponent.
///
/// This is the canonical definition. `DEFAULT_RESERVE_CAPACITY = 2^DEFAULT_RESERVE_CAPACITY_LOG2`
pub const DEFAULT_RESERVE_CAPACITY_LOG2: u32 = 22;

/// Default reserve capacity in number of chunks for a full node.
///
/// This is the target number of chunks a full storage node should hold.
/// When the node's reserve exceeds this capacity, it may trigger radius
/// adjustments to maintain the target.
///
/// Derived from: 2^22 = 4,194,304 chunks
///
/// At 4KB per chunk, this represents approximately 16 GB of chunk data
/// (not including metadata and indexing overhead).
pub const DEFAULT_RESERVE_CAPACITY: u64 = 1 << DEFAULT_RESERVE_CAPACITY_LOG2;

/// Default cache capacity for light nodes as a power of 2 exponent.
///
/// This is the canonical definition for memory-constrained devices.
/// `DEFAULT_CACHE_CAPACITY = 2^DEFAULT_CACHE_CAPACITY_LOG2`
pub const DEFAULT_CACHE_CAPACITY_LOG2: u32 = 16;

/// Default cache capacity in number of chunks for light nodes.
///
/// In-memory cache for retrieval/pushsync on non-storage nodes.
/// Suitable for memory-constrained devices.
///
/// Derived from: 2^16 = 65,536 chunks
///
/// At 4KB per chunk, this represents approximately 256 MB of memory.
pub const DEFAULT_CACHE_CAPACITY: u64 = 1 << DEFAULT_CACHE_CAPACITY_LOG2;

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
