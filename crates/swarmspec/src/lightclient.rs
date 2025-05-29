//! Incentives configuration for Swarm network

use alloy_primitives::U256;
use serde::{Deserialize, Serialize};

/// Configuration for pseudosettle (free bandwidth allocation)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// Configuration for settlement (payment channel)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementConfig {
    /// Minimum deposit amount in token base units
    pub min_deposit: U256,
    /// Minimum settlement threshold in token base units
    pub min_settlement_threshold: U256,
    /// Maximum settlement time in seconds
    pub settlement_timeout: u64,
    /// Whether to enforce settlement timeouts
    pub enforce_timeouts: bool,
}

impl Default for SettlementConfig {
    fn default() -> Self {
        Self {
            min_deposit: U256::from(1000),             // 1000 base units
            min_settlement_threshold: U256::from(100), // 100 base units
            settlement_timeout: 86400,                 // 24 hours
            enforce_timeouts: true,
        }
    }
}

/// Configuration for bandwidth incentives
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LightClient {
    /// Pseudosettle configuration for free bandwidth allocation
    pub pseudosettle: PseudosettleConfig,
    /// Settlement configuration for payment channels
    pub settlement: SettlementConfig,
    /// Minimum price per byte in wei
    pub min_price_per_byte: U256,
    /// Maximum price per byte in wei
    pub max_price_per_byte: U256,
    /// Default price per byte in wei
    pub default_price_per_byte: U256,
    /// Whether bandwidth incentives are enabled
    pub enabled: bool,
}

impl Default for LightClient {
    fn default() -> Self {
        Self {
            pseudosettle: PseudosettleConfig::default(),
            settlement: SettlementConfig::default(),
            min_price_per_byte: U256::from(1),    // 1 wei per byte
            max_price_per_byte: U256::from(1000), // 1000 wei per byte
            default_price_per_byte: U256::from(10), // 10 wei per byte
            enabled: true,
        }
    }
}
