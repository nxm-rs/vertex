//! Availability configuration for TOML persistence.

use serde::{Deserialize, Serialize};
use vertex_bandwidth_core::{
    DEFAULT_BASE_PRICE, DEFAULT_EARLY_PAYMENT_PERCENT, DEFAULT_LIGHT_FACTOR,
    DEFAULT_PAYMENT_THRESHOLD, DEFAULT_PAYMENT_TOLERANCE_PERCENT, DEFAULT_REFRESH_RATE,
};
use vertex_swarm_api::AvailabilityIncentiveConfig;

/// Availability incentive configuration (TOML-serializable).
///
/// All thresholds and prices are in **Accounting Units (AU)**, an abstract unit
/// used for bandwidth accounting. See `vertex-bandwidth-core` for details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvailabilityConfig {
    /// Whether to use pseudosettle (soft accounting)
    #[serde(default = "default_true")]
    pub pseudosettle_enabled: bool,

    /// Whether to use SWAP payment channels
    #[serde(default)]
    pub swap_enabled: bool,

    /// Payment threshold in accounting units.
    ///
    /// When a peer's debt reaches this threshold, settlement is requested.
    #[serde(default = "default_payment_threshold")]
    pub payment_threshold: u64,

    /// Payment tolerance as a percentage (0-100).
    ///
    /// Disconnect threshold = payment_threshold * (100 + tolerance) / 100.
    #[serde(default = "default_payment_tolerance_percent")]
    pub payment_tolerance_percent: u64,

    /// Base price per chunk in accounting units.
    ///
    /// Actual price depends on proximity: (MAX_PO - proximity + 1) * base_price.
    #[serde(default = "default_base_price")]
    pub base_price: u64,

    /// Refresh rate in accounting units per second.
    ///
    /// Used for pseudosettle time-based allowance.
    #[serde(default = "default_refresh_rate")]
    pub refresh_rate: u64,

    /// Early payment trigger percentage (0-100).
    ///
    /// Settlement is triggered when debt exceeds (100 - early)% of threshold.
    #[serde(default = "default_early_payment_percent")]
    pub early_payment_percent: u64,

    /// Light node scaling factor.
    ///
    /// Light nodes have all thresholds and rates divided by this factor.
    #[serde(default = "default_light_factor")]
    pub light_factor: u64,
}

impl Default for AvailabilityConfig {
    fn default() -> Self {
        Self {
            pseudosettle_enabled: true,
            swap_enabled: false,
            payment_threshold: DEFAULT_PAYMENT_THRESHOLD,
            payment_tolerance_percent: DEFAULT_PAYMENT_TOLERANCE_PERCENT,
            base_price: DEFAULT_BASE_PRICE,
            refresh_rate: DEFAULT_REFRESH_RATE,
            early_payment_percent: DEFAULT_EARLY_PAYMENT_PERCENT,
            light_factor: DEFAULT_LIGHT_FACTOR,
        }
    }
}

impl AvailabilityIncentiveConfig for AvailabilityConfig {
    fn pseudosettle_enabled(&self) -> bool {
        self.pseudosettle_enabled
    }

    fn swap_enabled(&self) -> bool {
        self.swap_enabled
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

fn default_true() -> bool {
    true
}

fn default_payment_threshold() -> u64 {
    DEFAULT_PAYMENT_THRESHOLD
}

fn default_payment_tolerance_percent() -> u64 {
    DEFAULT_PAYMENT_TOLERANCE_PERCENT
}

fn default_base_price() -> u64 {
    DEFAULT_BASE_PRICE
}

fn default_refresh_rate() -> u64 {
    DEFAULT_REFRESH_RATE
}

fn default_early_payment_percent() -> u64 {
    DEFAULT_EARLY_PAYMENT_PERCENT
}

fn default_light_factor() -> u64 {
    DEFAULT_LIGHT_FACTOR
}
