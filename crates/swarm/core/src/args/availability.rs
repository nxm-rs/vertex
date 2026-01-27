//! Availability incentive CLI arguments.

use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};
use vertex_bandwidth_core::{
    DEFAULT_BASE_PRICE, DEFAULT_EARLY_PAYMENT_PERCENT, DEFAULT_LIGHT_FACTOR,
    DEFAULT_PAYMENT_THRESHOLD, DEFAULT_PAYMENT_TOLERANCE_PERCENT, DEFAULT_REFRESH_RATE,
};
use vertex_swarm_api::AvailabilityIncentiveConfig;

/// Availability incentive mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AvailabilityMode {
    /// No availability accounting (dev/testing only).
    None,
    /// Soft accounting without real payments.
    #[default]
    Pseudosettle,
    /// Real payment channels with chequebook.
    Swap,
    /// Both pseudosettle and SWAP (SWAP when threshold reached).
    Both,
}

/// Availability incentive configuration.
///
/// All thresholds are in **Accounting Units (AU)**, matching Bee's accounting system.
/// Default values match Bee: payment threshold = 13,500,000 AU, tolerance = 25%.
#[derive(Debug, Args, Clone, Serialize, Deserialize)]
#[command(next_help_heading = "Availability Incentives")]
#[serde(default)]
pub struct AvailabilityArgs {
    /// Availability incentive mode.
    ///
    /// - none: No accounting (dev only)
    /// - pseudosettle: Soft accounting without payments (default)
    /// - swap: Real payments via SWAP chequebook
    /// - both: Pseudosettle until threshold, then SWAP
    #[arg(long = "availability.mode", value_enum, default_value_t = AvailabilityMode::Pseudosettle)]
    pub mode: AvailabilityMode,

    /// Payment threshold in accounting units.
    ///
    /// When a peer's debt reaches this threshold, settlement is requested.
    /// Default: 13,500,000 AU (matches Bee).
    #[arg(long = "availability.threshold", default_value_t = DEFAULT_PAYMENT_THRESHOLD)]
    pub payment_threshold: u64,

    /// Payment tolerance as a percentage (0-100).
    ///
    /// Disconnect threshold = payment_threshold * (100 + tolerance) / 100.
    /// Default: 25% (matches Bee).
    #[arg(long = "availability.tolerance-percent", default_value_t = DEFAULT_PAYMENT_TOLERANCE_PERCENT)]
    pub payment_tolerance_percent: u64,

    /// Base price per chunk in accounting units.
    ///
    /// Actual price depends on proximity: (31 - proximity + 1) * base_price.
    /// Default: 10,000 AU (matches Bee).
    #[arg(long = "availability.base-price", default_value_t = DEFAULT_BASE_PRICE)]
    pub base_price: u64,

    /// Refresh rate in accounting units per second.
    ///
    /// Used for pseudosettle time-based allowance.
    /// Default: 4,500,000 AU/s for full nodes (matches Bee).
    #[arg(long = "availability.refresh-rate", default_value_t = DEFAULT_REFRESH_RATE)]
    pub refresh_rate: u64,

    /// Early payment trigger percentage (0-100).
    ///
    /// Settlement is triggered when debt exceeds (100 - early)% of threshold.
    /// Default: 50% (matches Bee).
    #[arg(long = "availability.early-percent", default_value_t = DEFAULT_EARLY_PAYMENT_PERCENT)]
    pub early_payment_percent: u64,

    /// Light node scaling factor.
    ///
    /// Light nodes have all thresholds and rates divided by this factor.
    /// Default: 10 (matches Bee).
    #[arg(long = "availability.light-factor", default_value_t = DEFAULT_LIGHT_FACTOR)]
    pub light_factor: u64,
}

impl Default for AvailabilityArgs {
    fn default() -> Self {
        Self {
            mode: AvailabilityMode::default(),
            payment_threshold: DEFAULT_PAYMENT_THRESHOLD,
            payment_tolerance_percent: DEFAULT_PAYMENT_TOLERANCE_PERCENT,
            base_price: DEFAULT_BASE_PRICE,
            refresh_rate: DEFAULT_REFRESH_RATE,
            early_payment_percent: DEFAULT_EARLY_PAYMENT_PERCENT,
            light_factor: DEFAULT_LIGHT_FACTOR,
        }
    }
}

impl AvailabilityArgs {
    /// Validate argument combinations.
    ///
    /// Returns an error if arguments are set that don't apply to the selected mode.
    pub fn validate(&self) -> Result<(), String> {
        match self.mode {
            AvailabilityMode::None => {
                // No args should be non-default when mode is none
                let default = Self::default();
                if self.refresh_rate != default.refresh_rate
                    || self.payment_threshold != default.payment_threshold
                    || self.payment_tolerance_percent != default.payment_tolerance_percent
                    || self.base_price != default.base_price
                    || self.early_payment_percent != default.early_payment_percent
                    || self.light_factor != default.light_factor
                {
                    return Err(
                        "availability options have no effect when mode is 'none'".to_string()
                    );
                }
            }
            AvailabilityMode::Pseudosettle => {
                let default = Self::default();
                if self.early_payment_percent != default.early_payment_percent {
                    return Err(
                        "early-percent only applies to 'swap' or 'both' modes".to_string()
                    );
                }
            }
            AvailabilityMode::Swap => {
                let default = Self::default();
                if self.refresh_rate != default.refresh_rate {
                    return Err(
                        "refresh-rate only applies to 'pseudosettle' or 'both' modes".to_string()
                    );
                }
            }
            AvailabilityMode::Both => {
                // All args are valid
            }
        }
        Ok(())
    }
}

impl AvailabilityIncentiveConfig for AvailabilityArgs {
    fn pseudosettle_enabled(&self) -> bool {
        matches!(
            self.mode,
            AvailabilityMode::Pseudosettle | AvailabilityMode::Both
        )
    }

    fn swap_enabled(&self) -> bool {
        matches!(self.mode, AvailabilityMode::Swap | AvailabilityMode::Both)
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
