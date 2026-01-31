//! CLI arguments for pricing configuration.

use clap::Args;
use serde::{Deserialize, Serialize};
use vertex_swarm_api::SwarmPricingConfig;

use crate::constants::DEFAULT_BASE_PRICE;

/// Chunk pricing CLI arguments.
#[derive(Debug, Args, Clone, Serialize, Deserialize)]
#[command(next_help_heading = "Bandwidth Pricing")]
#[serde(default)]
pub struct PricingArgs {
    /// Base price per chunk (scaled by proximity for peer pricing)
    #[arg(long = "bandwidth.base-price", default_value_t = DEFAULT_BASE_PRICE)]
    pub base_price: u64,
}

impl Default for PricingArgs {
    fn default() -> Self {
        Self {
            base_price: DEFAULT_BASE_PRICE,
        }
    }
}

impl SwarmPricingConfig for PricingArgs {
    fn base_price(&self) -> u64 {
        self.base_price
    }
}
