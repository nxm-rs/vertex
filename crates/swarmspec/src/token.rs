//! Definition of the Swarm token

use alloy_primitives::Address;
use serde::{Deserialize, Serialize};

/// Swarm token (BZZ) details
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

impl Default for Token {
    fn default() -> Self {
        Self {
            address: Address::ZERO,
            name: "Dev Swarm",
            symbol: "dBZZ",
            decimals: 16,
        }
    }
}
