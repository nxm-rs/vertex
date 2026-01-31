//! CLI arguments for bandwidth accounting configuration.

use clap::Args;
use serde::{Deserialize, Serialize};
use vertex_swarm_api::SwarmAccountingConfig;
use vertex_swarm_primitives::BandwidthMode;

use crate::constants::*;

/// CLI wrapper for [`BandwidthMode`] with clap integration.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum, strum::FromRepr, Serialize, Deserialize,
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

/// Bandwidth accounting CLI arguments. All thresholds are in AU.
#[derive(Debug, Args, Clone, Serialize, Deserialize)]
#[command(next_help_heading = "Bandwidth Accounting")]
#[serde(default)]
pub struct BandwidthArgs {
    /// Bandwidth accounting mode
    #[arg(long = "bandwidth.mode", value_enum, default_value_t = BandwidthModeArg::Pseudosettle)]
    pub mode: BandwidthModeArg,

    /// Payment threshold (triggers settlement when exceeded)
    #[arg(long = "bandwidth.threshold", default_value_t = DEFAULT_PAYMENT_THRESHOLD)]
    pub payment_threshold: u64,

    /// Payment tolerance percent for disconnect threshold
    #[arg(long = "bandwidth.tolerance-percent", default_value_t = DEFAULT_PAYMENT_TOLERANCE_PERCENT)]
    pub payment_tolerance_percent: u64,

    /// Base price per chunk (scaled by proximity)
    #[arg(long = "bandwidth.base-price", default_value_t = DEFAULT_BASE_PRICE)]
    pub base_price: u64,

    /// Pseudosettle refresh rate per second
    #[arg(long = "bandwidth.refresh-rate", default_value_t = DEFAULT_REFRESH_RATE)]
    pub refresh_rate: u64,

    /// Early payment trigger percent (for SWAP)
    #[arg(long = "bandwidth.early-percent", default_value_t = DEFAULT_EARLY_PAYMENT_PERCENT)]
    pub early_payment_percent: u64,

    /// Scaling factor for client-only nodes (divides thresholds)
    #[arg(long = "bandwidth.client-only-factor", default_value_t = DEFAULT_CLIENT_ONLY_FACTOR)]
    pub client_only_factor: u64,
}

impl Default for BandwidthArgs {
    fn default() -> Self {
        Self {
            mode: BandwidthModeArg::default(),
            payment_threshold: DEFAULT_PAYMENT_THRESHOLD,
            payment_tolerance_percent: DEFAULT_PAYMENT_TOLERANCE_PERCENT,
            base_price: DEFAULT_BASE_PRICE,
            refresh_rate: DEFAULT_REFRESH_RATE,
            early_payment_percent: DEFAULT_EARLY_PAYMENT_PERCENT,
            client_only_factor: DEFAULT_CLIENT_ONLY_FACTOR,
        }
    }
}

impl BandwidthArgs {
    /// Validate argument combinations.
    ///
    /// Returns an error if arguments are set that don't apply to the selected mode.
    pub fn validate(&self) -> Result<(), String> {
        match self.mode {
            BandwidthModeArg::None => {
                let default = Self::default();
                if self.refresh_rate != default.refresh_rate
                    || self.payment_threshold != default.payment_threshold
                    || self.payment_tolerance_percent != default.payment_tolerance_percent
                    || self.base_price != default.base_price
                    || self.early_payment_percent != default.early_payment_percent
                    || self.client_only_factor != default.client_only_factor
                {
                    return Err("bandwidth options have no effect when mode is 'none'".to_string());
                }
            }
            BandwidthModeArg::Pseudosettle => {
                let default = Self::default();
                if self.early_payment_percent != default.early_payment_percent {
                    return Err("early-percent only applies to 'swap' or 'both' modes".to_string());
                }
            }
            BandwidthModeArg::Swap => {
                let default = Self::default();
                if self.refresh_rate != default.refresh_rate {
                    return Err(
                        "refresh-rate only applies to 'pseudosettle' or 'both' modes".to_string(),
                    );
                }
            }
            BandwidthModeArg::Both => {
                // All args are valid
            }
        }
        Ok(())
    }
}

impl SwarmAccountingConfig for BandwidthArgs {
    fn mode(&self) -> BandwidthMode {
        self.mode.into()
    }

    fn payment_threshold(&self) -> u64 {
        self.payment_threshold
    }

    fn payment_tolerance_percent(&self) -> u64 {
        self.payment_tolerance_percent
    }

    fn base_price(&self) -> u64 {
        self.base_price
    }

    fn refresh_rate(&self) -> u64 {
        self.refresh_rate
    }

    fn early_payment_percent(&self) -> u64 {
        self.early_payment_percent
    }

    fn client_only_factor(&self) -> u64 {
        self.client_only_factor
    }
}
