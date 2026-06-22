//! CLI arguments for bandwidth accounting configuration.

use clap::Args;
use serde::{Deserialize, Serialize};

pub use vertex_swarm_accounting_pricing::FixedPricingArgs;

use crate::constants::*;

/// Bandwidth accounting CLI arguments.
///
/// This struct is for CLI parsing and serialization only.
/// Convert to `BandwidthConfig` for runtime use.
#[derive(Debug, Args, Clone, Serialize, Deserialize)]
#[command(next_help_heading = "Bandwidth Accounting")]
#[serde(default)]
pub struct BandwidthArgs {
    /// Payment threshold (triggers settlement when exceeded).
    #[arg(long = "bandwidth.threshold", default_value_t = DEFAULT_PAYMENT_THRESHOLD)]
    pub payment_threshold: u64,

    /// Payment tolerance percent for disconnect threshold.
    #[arg(long = "bandwidth.tolerance-percent", default_value_t = DEFAULT_PAYMENT_TOLERANCE_PERCENT)]
    pub payment_tolerance_percent: u64,

    /// Pseudosettle refresh rate per second.
    #[arg(long = "bandwidth.refresh-rate", default_value_t = DEFAULT_REFRESH_RATE)]
    pub refresh_rate: u64,

    /// Early payment trigger percent (for SWAP).
    #[arg(long = "bandwidth.early-percent", default_value_t = DEFAULT_EARLY_PAYMENT_PERCENT)]
    pub early_payment_percent: u64,

    /// Scaling factor for client-only nodes (divides thresholds).
    #[arg(long = "bandwidth.client-only-factor", default_value_t = DEFAULT_CLIENT_ONLY_FACTOR)]
    pub client_only_factor: u64,

    /// Percent (1..=100) of the payment-threshold headroom the outbound
    /// self-throttle will consume. Leaves a margin below the settlement trigger.
    #[arg(long = "bandwidth.throttle-allowance-percent", default_value_t = DEFAULT_THROTTLE_ALLOWANCE_PERCENT)]
    pub throttle_allowance_percent: u8,

    /// Chunk pricing configuration.
    #[command(flatten)]
    #[serde(default)]
    pub pricing: FixedPricingArgs,
}

impl Default for BandwidthArgs {
    fn default() -> Self {
        Self {
            payment_threshold: DEFAULT_PAYMENT_THRESHOLD,
            payment_tolerance_percent: DEFAULT_PAYMENT_TOLERANCE_PERCENT,
            refresh_rate: DEFAULT_REFRESH_RATE,
            early_payment_percent: DEFAULT_EARLY_PAYMENT_PERCENT,
            client_only_factor: DEFAULT_CLIENT_ONLY_FACTOR,
            throttle_allowance_percent: DEFAULT_THROTTLE_ALLOWANCE_PERCENT,
            pricing: FixedPricingArgs::default(),
        }
    }
}

impl BandwidthArgs {
    /// Create validated BandwidthConfig from these CLI arguments.
    pub fn accounting_config(
        &self,
    ) -> Result<crate::DefaultBandwidthConfig, crate::BandwidthConfigError> {
        crate::BandwidthConfig::try_from(self)
    }
}
