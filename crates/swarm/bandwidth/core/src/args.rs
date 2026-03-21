//! CLI arguments for bandwidth accounting configuration.

use clap::Args;
use serde::{Deserialize, Serialize};
use vertex_swarm_primitives::BandwidthMode;

pub use vertex_swarm_bandwidth_pricing::FixedPricingArgs;

use crate::constants::*;

/// CLI wrapper for [`BandwidthMode`] with clap integration.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    clap::ValueEnum,
    strum::FromRepr,
    Serialize,
    Deserialize,
)]
#[serde(rename_all = "lowercase")]
#[repr(u8)]
pub enum BandwidthModeArg {
    /// No bandwidth accounting (dev only).
    None = 0,
    /// Soft accounting without real payments (default).
    #[default]
    Pseudosettle = 1,
    /// Real payment channels with chequebook.
    Swap = 2,
    /// Both pseudosettle and SWAP.
    Both = 3,
}

impl From<BandwidthModeArg> for BandwidthMode {
    fn from(arg: BandwidthModeArg) -> Self {
        BandwidthMode::from_repr(arg as u8).expect("matching repr")
    }
}

impl From<BandwidthMode> for BandwidthModeArg {
    fn from(mode: BandwidthMode) -> Self {
        BandwidthModeArg::from_repr(mode as u8).expect("matching repr")
    }
}

/// Bandwidth accounting CLI arguments.
///
/// This struct is for CLI parsing and serialization only.
/// Convert to `BandwidthConfig` for runtime use.
#[derive(Debug, Args, Clone, Serialize, Deserialize)]
#[command(next_help_heading = "Bandwidth Accounting")]
#[serde(default)]
pub struct BandwidthArgs {
    /// Bandwidth accounting mode.
    #[arg(long = "bandwidth.mode", value_enum, default_value_t = BandwidthModeArg::Pseudosettle)]
    pub mode: BandwidthModeArg,

    /// Credit limit (triggers settlement when exceeded).
    #[arg(long = "bandwidth.threshold", default_value_t = DEFAULT_CREDIT_LIMIT)]
    pub credit_limit: u64,

    /// Credit tolerance percent for disconnect limit.
    #[arg(long = "bandwidth.tolerance-percent", default_value_t = DEFAULT_CREDIT_TOLERANCE_PERCENT)]
    pub credit_tolerance_percent: u64,

    /// Pseudosettle refresh rate per second.
    #[arg(long = "bandwidth.refresh-rate", default_value_t = DEFAULT_REFRESH_RATE)]
    pub refresh_rate: u64,

    /// Early payment trigger percent (for SWAP).
    #[arg(long = "bandwidth.early-percent", default_value_t = DEFAULT_EARLY_PAYMENT_PERCENT)]
    pub early_payment_percent: u64,

    /// Scaling factor for client-only nodes (divides thresholds).
    #[arg(long = "bandwidth.client-only-factor", default_value_t = DEFAULT_CLIENT_ONLY_FACTOR)]
    pub client_only_factor: u64,

    /// Chunk pricing configuration.
    #[command(flatten)]
    #[serde(default)]
    pub pricing: FixedPricingArgs,
}

impl Default for BandwidthArgs {
    fn default() -> Self {
        Self {
            mode: BandwidthModeArg::default(),
            credit_limit: DEFAULT_CREDIT_LIMIT,
            credit_tolerance_percent: DEFAULT_CREDIT_TOLERANCE_PERCENT,
            refresh_rate: DEFAULT_REFRESH_RATE,
            early_payment_percent: DEFAULT_EARLY_PAYMENT_PERCENT,
            client_only_factor: DEFAULT_CLIENT_ONLY_FACTOR,
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
