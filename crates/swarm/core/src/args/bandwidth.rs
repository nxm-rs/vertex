//! Bandwidth incentive CLI arguments.

use clap::Args;
use serde::{Deserialize, Serialize};
use vertex_swarm_api::SwarmAccountingConfig;
use vertex_swarm_primitives::BandwidthMode;

/// CLI argument type for bandwidth mode selection.
///
/// This is a CLI-specific wrapper around [`BandwidthMode`] that provides
/// clap integration. Use `.into()` to convert to [`BandwidthMode`].
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

/// Default base price per chunk in accounting units.
const DEFAULT_BASE_PRICE: u64 = 10_000;

/// Default refresh rate in accounting units per second.
const DEFAULT_REFRESH_RATE: u64 = 4_500_000;

/// Default payment threshold in accounting units.
const DEFAULT_PAYMENT_THRESHOLD: u64 = 13_500_000;

/// Default payment tolerance as a percentage.
const DEFAULT_PAYMENT_TOLERANCE_PERCENT: u64 = 25;

/// Default early payment trigger percentage.
const DEFAULT_EARLY_PAYMENT_PERCENT: u64 = 50;

/// Default light node scaling factor.
const DEFAULT_LIGHT_FACTOR: u64 = 10;

/// Bandwidth incentive configuration.
///
/// All thresholds are in **Accounting Units (AU)**.
#[derive(Debug, Args, Clone, Serialize, Deserialize)]
#[command(next_help_heading = "Bandwidth Incentives")]
#[serde(default)]
pub struct BandwidthArgs {
    /// Bandwidth incentive mode.
    ///
    /// - none: No accounting (dev only)
    /// - pseudosettle: Soft accounting without payments (default)
    /// - swap: Real payments via SWAP chequebook
    /// - both: Pseudosettle until threshold, then SWAP
    #[arg(long = "bandwidth.mode", value_enum, default_value_t = BandwidthModeArg::Pseudosettle)]
    pub mode: BandwidthModeArg,

    /// Payment threshold in accounting units.
    ///
    /// When a peer's debt reaches this threshold, settlement is requested.
    #[arg(long = "bandwidth.threshold", default_value_t = DEFAULT_PAYMENT_THRESHOLD)]
    pub payment_threshold: u64,

    /// Payment tolerance as a percentage (0-100).
    ///
    /// Disconnect threshold = payment_threshold * (100 + tolerance) / 100.
    #[arg(long = "bandwidth.tolerance-percent", default_value_t = DEFAULT_PAYMENT_TOLERANCE_PERCENT)]
    pub payment_tolerance_percent: u64,

    /// Base price per chunk in accounting units.
    ///
    /// Actual price depends on proximity: (31 - proximity + 1) * base_price.
    #[arg(long = "bandwidth.base-price", default_value_t = DEFAULT_BASE_PRICE)]
    pub base_price: u64,

    /// Refresh rate in accounting units per second.
    ///
    /// Used for pseudosettle time-based allowance.
    #[arg(long = "bandwidth.refresh-rate", default_value_t = DEFAULT_REFRESH_RATE)]
    pub refresh_rate: u64,

    /// Early payment trigger percentage (0-100).
    ///
    /// Settlement is triggered when debt exceeds (100 - early)% of threshold.
    #[arg(long = "bandwidth.early-percent", default_value_t = DEFAULT_EARLY_PAYMENT_PERCENT)]
    pub early_payment_percent: u64,

    /// Client node scaling factor.
    ///
    /// Client nodes have all thresholds and rates divided by this factor.
    #[arg(long = "bandwidth.light-factor", default_value_t = DEFAULT_LIGHT_FACTOR)]
    pub light_factor: u64,
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
            light_factor: DEFAULT_LIGHT_FACTOR,
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
                // No args should be non-default when mode is none
                let default = Self::default();
                if self.refresh_rate != default.refresh_rate
                    || self.payment_threshold != default.payment_threshold
                    || self.payment_tolerance_percent != default.payment_tolerance_percent
                    || self.base_price != default.base_price
                    || self.early_payment_percent != default.early_payment_percent
                    || self.light_factor != default.light_factor
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

    fn light_factor(&self) -> u64 {
        self.light_factor
    }
}
