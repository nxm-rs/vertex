//! Validated bandwidth accounting configuration.

use vertex_swarm_accounting_pricing::FixedPricingConfig;
use vertex_swarm_api::{Au, SwarmAccountingConfig, SwarmPricingConfig};

use crate::args::BandwidthArgs;
use crate::constants::*;

/// Error during bandwidth configuration validation.
#[derive(Debug, Clone, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum BandwidthConfigError {
    #[error("throttle-allowance-percent must be in 1..=100")]
    ThrottleAllowancePercentOutOfRange,
}

/// Validated bandwidth accounting configuration.
///
/// Generic over the pricing configuration type `P`. Use [`DefaultBandwidthConfig`]
/// for the standard CLI-produced configuration with fixed pricing.
#[derive(Debug, Clone)]
pub struct BandwidthConfig<P = FixedPricingConfig> {
    payment_threshold: u64,
    payment_tolerance_percent: u64,
    refresh_rate: u64,
    early_payment_percent: u64,
    client_only_factor: u64,
    throttle_allowance_percent: u8,
    pricing: P,
}

/// Default bandwidth config using fixed pricing (CLI-produced).
pub type DefaultBandwidthConfig = BandwidthConfig<FixedPricingConfig>;

impl<P> BandwidthConfig<P> {
    /// Create with explicit values.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        payment_threshold: u64,
        payment_tolerance_percent: u64,
        refresh_rate: u64,
        early_payment_percent: u64,
        client_only_factor: u64,
        throttle_allowance_percent: u8,
        pricing: P,
    ) -> Self {
        Self {
            payment_threshold,
            payment_tolerance_percent,
            refresh_rate,
            early_payment_percent,
            client_only_factor,
            throttle_allowance_percent,
            pricing,
        }
    }

    /// Get the pricing configuration.
    pub fn pricing(&self) -> &P {
        &self.pricing
    }

    /// Percent (1..=100) of the payment-threshold headroom the outbound
    /// self-throttle will consume, leaving a margin below the settlement
    /// trigger.
    pub fn throttle_allowance_percent(&self) -> u8 {
        self.throttle_allowance_percent
    }
}

impl TryFrom<&BandwidthArgs> for BandwidthConfig<FixedPricingConfig> {
    type Error = BandwidthConfigError;

    fn try_from(args: &BandwidthArgs) -> Result<Self, Self::Error> {
        if !(1..=100).contains(&args.throttle_allowance_percent) {
            return Err(BandwidthConfigError::ThrottleAllowancePercentOutOfRange);
        }

        Ok(Self {
            payment_threshold: args.payment_threshold,
            payment_tolerance_percent: args.payment_tolerance_percent,
            refresh_rate: args.refresh_rate,
            early_payment_percent: args.early_payment_percent,
            client_only_factor: args.client_only_factor,
            throttle_allowance_percent: args.throttle_allowance_percent,
            pricing: FixedPricingConfig::from(&args.pricing),
        })
    }
}

impl Default for BandwidthConfig<FixedPricingConfig> {
    fn default() -> Self {
        Self {
            payment_threshold: DEFAULT_PAYMENT_THRESHOLD,
            payment_tolerance_percent: DEFAULT_PAYMENT_TOLERANCE_PERCENT,
            refresh_rate: DEFAULT_REFRESH_RATE,
            early_payment_percent: DEFAULT_EARLY_PAYMENT_PERCENT,
            client_only_factor: DEFAULT_CLIENT_ONLY_FACTOR,
            throttle_allowance_percent: DEFAULT_THROTTLE_ALLOWANCE_PERCENT,
            pricing: FixedPricingConfig::default(),
        }
    }
}

impl<P> SwarmAccountingConfig for BandwidthConfig<P>
where
    P: Send + Sync,
{
    fn payment_threshold(&self) -> Au {
        Au::from_amount(self.payment_threshold)
    }

    fn payment_tolerance_percent(&self) -> u64 {
        self.payment_tolerance_percent
    }

    fn refresh_rate(&self) -> Au {
        Au::from_amount(self.refresh_rate)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_throttle_allowance_percent() {
        let config = DefaultBandwidthConfig::default();
        assert_eq!(
            config.throttle_allowance_percent(),
            DEFAULT_THROTTLE_ALLOWANCE_PERCENT
        );
    }

    #[test]
    fn throttle_allowance_percent_in_range_is_accepted() {
        for pct in [1u8, 50, 85, 100] {
            let args = BandwidthArgs {
                throttle_allowance_percent: pct,
                ..BandwidthArgs::default()
            };
            let config = BandwidthConfig::try_from(&args).expect("valid percent");
            assert_eq!(config.throttle_allowance_percent(), pct);
        }
    }

    #[test]
    fn throttle_allowance_percent_out_of_range_is_rejected() {
        for pct in [0u8, 101, 200] {
            let args = BandwidthArgs {
                throttle_allowance_percent: pct,
                ..BandwidthArgs::default()
            };
            assert!(matches!(
                BandwidthConfig::try_from(&args),
                Err(BandwidthConfigError::ThrottleAllowancePercentOutOfRange)
            ));
        }
    }
}
