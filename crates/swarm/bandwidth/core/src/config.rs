//! Validated bandwidth accounting configuration.

use vertex_swarm_api::{BandwidthMode, SwarmAccountingConfig};

use crate::args::{BandwidthArgs, BandwidthModeArg};
use crate::constants::*;

/// Error during bandwidth configuration validation.
#[derive(Debug, Clone, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum BandwidthConfigError {
    #[error("bandwidth options have no effect when mode is 'none'")]
    OptionsWithDisabledMode,
    #[error("early-percent only applies to 'swap' or 'both' modes")]
    EarlyPercentWithoutSwap,
    #[error("refresh-rate only applies to 'pseudosettle' or 'both' modes")]
    RefreshRateWithoutPseudosettle,
}

/// Validated bandwidth accounting configuration.
#[derive(Debug, Clone)]
pub struct BandwidthConfig {
    mode: BandwidthMode,
    credit_limit: u64,
    credit_tolerance_percent: u64,
    refresh_rate: u64,
    early_payment_percent: u64,
    client_only_factor: u64,
}

impl BandwidthConfig {
    /// Create with explicit values.
    pub fn new(
        mode: BandwidthMode,
        credit_limit: u64,
        credit_tolerance_percent: u64,
        refresh_rate: u64,
        early_payment_percent: u64,
        client_only_factor: u64,
    ) -> Self {
        Self {
            mode,
            credit_limit,
            credit_tolerance_percent,
            refresh_rate,
            early_payment_percent,
            client_only_factor,
        }
    }
}

impl TryFrom<&BandwidthArgs> for BandwidthConfig {
    type Error = BandwidthConfigError;

    fn try_from(args: &BandwidthArgs) -> Result<Self, Self::Error> {
        let default = BandwidthArgs::default();

        match args.mode {
            BandwidthModeArg::None => {
                let has_non_default = args.refresh_rate != default.refresh_rate
                    || args.credit_limit != default.credit_limit
                    || args.credit_tolerance_percent != default.credit_tolerance_percent
                    || args.early_payment_percent != default.early_payment_percent
                    || args.client_only_factor != default.client_only_factor;
                if has_non_default {
                    return Err(BandwidthConfigError::OptionsWithDisabledMode);
                }
            }
            BandwidthModeArg::Pseudosettle => {
                if args.early_payment_percent != default.early_payment_percent {
                    return Err(BandwidthConfigError::EarlyPercentWithoutSwap);
                }
            }
        }

        Ok(Self {
            mode: args.mode.into(),
            credit_limit: args.credit_limit,
            credit_tolerance_percent: args.credit_tolerance_percent,
            refresh_rate: args.refresh_rate,
            early_payment_percent: args.early_payment_percent,
            client_only_factor: args.client_only_factor,
        })
    }
}

impl Default for BandwidthConfig {
    fn default() -> Self {
        Self {
            mode: BandwidthMode::Pseudosettle,
            credit_limit: DEFAULT_CREDIT_LIMIT,
            credit_tolerance_percent: DEFAULT_CREDIT_TOLERANCE_PERCENT,
            refresh_rate: DEFAULT_REFRESH_RATE,
            early_payment_percent: DEFAULT_EARLY_PAYMENT_PERCENT,
            client_only_factor: DEFAULT_CLIENT_ONLY_FACTOR,
        }
    }
}

impl SwarmAccountingConfig for BandwidthConfig {
    fn mode(&self) -> BandwidthMode {
        self.mode
    }

    fn credit_limit(&self) -> u64 {
        self.credit_limit
    }

    fn credit_tolerance_percent(&self) -> u64 {
        self.credit_tolerance_percent
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
