//! Definition of the Swarm token

use alloy_primitives::Address;
use serde::{Deserialize, Serialize};
use vertex_swarm_api::SwarmToken;

/// Swarm token (BZZ) details
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
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

impl SwarmToken for Token {
    fn address(&self) -> Address {
        self.address
    }

    fn name(&self) -> &str {
        self.name
    }

    fn symbol(&self) -> &str {
        self.symbol
    }

    fn decimals(&self) -> u8 {
        self.decimals
    }
}
