//! CLI arguments for fixed pricing configuration.

use clap::Args;
use serde::{Deserialize, Serialize};

use crate::constants::DEFAULT_BASE_PRICE;

/// Fixed-rate chunk pricing CLI arguments.
#[derive(Debug, Args, Clone, Serialize, Deserialize)]
#[command(next_help_heading = "Bandwidth Pricing")]
#[serde(default)]
pub struct FixedPricingArgs {
    /// Base price per chunk (scaled by proximity for peer pricing).
    #[arg(long = "bandwidth.base-price", default_value_t = DEFAULT_BASE_PRICE)]
    pub base_price: u64,
}

impl Default for FixedPricingArgs {
    fn default() -> Self {
        Self {
            base_price: DEFAULT_BASE_PRICE,
        }
    }
}
