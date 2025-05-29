//! Common types used in Swarm network specification

use core::fmt::Debug;
use libp2p::Multiaddr;
use vertex_primitives::Address;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Swarm token (BZZ) contract details
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Token {
    /// Contract address
    pub address: Address,
    /// Token name
    pub name: &'static str,
    /// Token symbol
    pub symbol: &'static str,
    /// Decimal places
    pub decimals: u8,
}

/// Storage contract addresses
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct StorageContracts {
    /// Postage stamp contract address
    pub postage: Address,
    /// Redistribution contract address
    pub redistribution: Address,
    /// Optional staking contract address
    pub staking: Option<Address>,
}

/// Configuration for pseudosettle (free bandwidth allocation)
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct PseudosettleConfig {
    /// Daily free bandwidth allowance in bytes
    pub daily_allowance_bytes: u64,
    /// Threshold after which payment is required (bytes)
    pub payment_threshold: u64,
    /// Payment tolerance before disconnection (bytes)
    pub payment_tolerance: u64,
    /// Disconnect threshold (bytes)
    pub disconnect_threshold: u64,
}

impl Default for PseudosettleConfig {
    fn default() -> Self {
        Self {
            daily_allowance_bytes: 1_000_000, // 1MB per day free
            payment_threshold: 10_000_000,    // 10MB
            payment_tolerance: 5_000_000,     // 5MB
            disconnect_threshold: 50_000_000, // 50MB
        }
    }
}

/// Configuration for node storage parameters
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Storage {
    /// Smart contract addresses for storage incentives
    pub contracts: StorageContracts,
    /// Maximum number of chunks the node will store
    pub max_chunks: u64,
    /// Target number of chunks for optimal operation
    pub target_chunks: u64,
    /// Minimum number of chunks before node considers scaling down
    pub min_chunks: u64,
    /// Reserve storage percentage (0-100) before starting to evict content
    pub reserve_percentage: u8,
    /// Chunk size in bytes
    pub chunk_size: u64,
    /// Time (in seconds) to wait before a node tries to scale up the neighborhood
    pub scale_up_interval: u64,
    /// Time (in seconds) to wait before a node tries to scale down the neighborhood
    pub scale_down_interval: u64,
}

impl Default for Storage {
    fn default() -> Self {
        Self {
            contracts: StorageContracts {
                postage: Address::ZERO,
                redistribution: Address::ZERO,
                staking: Some(Address::ZERO),
            },
            max_chunks: 1_000_000,      // 1 million chunks
            target_chunks: 500_000,     // 500k chunks
            min_chunks: 100_000,        // 100k chunks
            reserve_percentage: 10,     // 10% reserve
            chunk_size: 4096,           // 4KB chunks
            scale_up_interval: 86400,   // 24 hours
            scale_down_interval: 86400, // 24 hours
        }
    }
}
