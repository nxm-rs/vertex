//! Validated bandwidth accounting configuration.

use vertex_swarm_api::{BandwidthMode, SwarmAccountingConfig, SwarmPricingConfig};
use vertex_swarm_bandwidth_pricing::FixedPricingConfig;

use crate::args::{BandwidthArgs, BandwidthModeArg};
use crate::constants::*;

/// Error during bandwidth configuration validation.
#[derive(Debug, Clone, thiserror::Error)]
pub enum BandwidthConfigError {
    #[error("bandwidth options have no effect when mode is 'none'")]
    OptionsWithDisabledMode,
    #[error("early-percent only applies to 'swap' or 'both' modes")]
    EarlyPercentWithoutSwap,
    #[error("refresh-rate only applies to 'pseudosettle' or 'both' modes")]
    RefreshRateWithoutPseudosettle,
}

/// Validated bandwidth accounting configuration.
///
/// Generic over the pricing configuration type `P`. Use [`DefaultBandwidthConfig`]
/// for the standard CLI-produced configuration with fixed pricing.
#[derive(Debug, Clone)]
pub struct BandwidthConfig<P = FixedPricingConfig> {
    mode: BandwidthMode,
    payment_threshold: u64,
    payment_tolerance_percent: u64,
    refresh_rate: u64,
    early_payment_percent: u64,
    client_only_factor: u64,
    pricing: P,
}

/// Default bandwidth config using fixed pricing (CLI-produced).
pub type DefaultBandwidthConfig = BandwidthConfig<FixedPricingConfig>;

impl<P> BandwidthConfig<P> {
    /// Create with explicit values.
    pub fn new(
        mode: BandwidthMode,
        payment_threshold: u64,
        payment_tolerance_percent: u64,
        refresh_rate: u64,
        early_payment_percent: u64,
        client_only_factor: u64,
        pricing: P,
    ) -> Self {
        Self {
            mode,
            payment_threshold,
            payment_tolerance_percent,
            refresh_rate,
            early_payment_percent,
            client_only_factor,
            pricing,
        }
    }

    /// Get the pricing configuration.
    pub fn pricing(&self) -> &P {
        &self.pricing
    }
}

impl TryFrom<&BandwidthArgs> for BandwidthConfig<FixedPricingConfig> {
    type Error = BandwidthConfigError;

    fn try_from(args: &BandwidthArgs) -> Result<Self, Self::Error> {
        let default = BandwidthArgs::default();

        match args.mode {
            BandwidthModeArg::None => {
                let has_non_default = args.refresh_rate != default.refresh_rate
                    || args.payment_threshold != default.payment_threshold
                    || args.payment_tolerance_percent != default.payment_tolerance_percent
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
            BandwidthModeArg::Swap => {
                if args.refresh_rate != default.refresh_rate {
                    return Err(BandwidthConfigError::RefreshRateWithoutPseudosettle);
                }
            }
            BandwidthModeArg::Both => {}
        }

        Ok(Self {
            mode: args.mode.into(),
            payment_threshold: args.payment_threshold,
            payment_tolerance_percent: args.payment_tolerance_percent,
            refresh_rate: args.refresh_rate,
            early_payment_percent: args.early_payment_percent,
            client_only_factor: args.client_only_factor,
            pricing: FixedPricingConfig::from(&args.pricing),
        })
    }
}

impl Default for BandwidthConfig<FixedPricingConfig> {
    fn default() -> Self {
        Self {
            mode: BandwidthMode::Pseudosettle,
            payment_threshold: DEFAULT_PAYMENT_THRESHOLD,
            payment_tolerance_percent: DEFAULT_PAYMENT_TOLERANCE_PERCENT,
            refresh_rate: DEFAULT_REFRESH_RATE,
            early_payment_percent: DEFAULT_EARLY_PAYMENT_PERCENT,
            client_only_factor: DEFAULT_CLIENT_ONLY_FACTOR,
            pricing: FixedPricingConfig::default(),
        }
    }
}

impl<P> SwarmAccountingConfig for BandwidthConfig<P>
where
    P: Send + Sync,
{
    fn mode(&self) -> BandwidthMode {
        self.mode
    }

    fn payment_threshold(&self) -> u64 {
        self.payment_threshold
    }

    fn payment_tolerance_percent(&self) -> u64 {
        self.payment_tolerance_percent
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

impl<P> SwarmPricingConfig for BandwidthConfig<P>
where
    P: Default + Clone + Send + Sync,
{
    type Pricing = P;

    fn pricing(&self) -> &P {
        &self.pricing
    }
}
