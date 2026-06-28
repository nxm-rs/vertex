//! Validated bandwidth accounting configuration.

use vertex_swarm_accounting_pricing::FixedPricingConfig;
use vertex_swarm_api::{Au, SwarmAccountingConfig, SwarmPricingConfig};

use crate::args::BandwidthArgs;
use crate::constants::*;

/// Bandwidth accounting configuration.
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
        pricing: P,
    ) -> Self {
        Self {
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

    /// This config scaled to the line a storer enforces on a client:
    /// `payment_threshold` and `refresh_rate` divided by `client_only_factor`,
    /// floored at one. Pacing against the unscaled storer figures would let a
    /// burst cross the storer's disconnect line before our settle engages.
    pub fn for_client(self) -> Self {
        let factor = self.client_only_factor.max(1);
        Self {
            payment_threshold: (self.payment_threshold / factor).max(1),
            refresh_rate: (self.refresh_rate / factor).max(1),
            ..self
        }
    }
}

impl From<&BandwidthArgs> for BandwidthConfig<FixedPricingConfig> {
    fn from(args: &BandwidthArgs) -> Self {
        Self {
            payment_threshold: args.payment_threshold,
            payment_tolerance_percent: args.payment_tolerance_percent,
            refresh_rate: args.refresh_rate,
            early_payment_percent: args.early_payment_percent,
            client_only_factor: args.client_only_factor,
            pricing: FixedPricingConfig::from(&args.pricing),
        }
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
    fn from_args_carries_the_thresholds() {
        let config = BandwidthConfig::from(&BandwidthArgs::default());
        assert_eq!(
            config.payment_threshold().as_amount(),
            DEFAULT_PAYMENT_THRESHOLD
        );
        assert_eq!(config.refresh_rate().as_amount(), DEFAULT_REFRESH_RATE);
        assert_eq!(config.client_only_factor(), DEFAULT_CLIENT_ONLY_FACTOR);
    }

    #[test]
    fn for_client_scales_threshold_and_refresh_by_the_factor() {
        let storer = DefaultBandwidthConfig::default();
        let factor = storer.client_only_factor();
        let storer_threshold = storer.payment_threshold().as_amount();
        let storer_refresh = storer.refresh_rate().as_amount();
        let storer_disconnect = storer.disconnect_threshold();
        let storer_tolerance = storer.payment_tolerance_percent();

        let client = storer.for_client();
        assert_eq!(
            client.payment_threshold().as_amount(),
            storer_threshold / factor
        );
        assert_eq!(client.refresh_rate().as_amount(), storer_refresh / factor);
        // The disconnect threshold derives from the now-scaled payment threshold,
        // so it scales down with it: we pace against the same client ceiling the
        // serving storer enforces on us.
        assert!(client.disconnect_threshold() < storer_disconnect);
        assert_eq!(client.payment_tolerance_percent(), storer_tolerance);
        assert_eq!(client.client_only_factor(), factor);
    }

    #[test]
    fn for_client_floors_at_one() {
        let cfg = BandwidthConfig {
            payment_threshold: 5,
            refresh_rate: 5,
            client_only_factor: 1000,
            ..DefaultBandwidthConfig::default()
        }
        .for_client();
        assert_eq!(cfg.payment_threshold().as_amount(), 1);
        assert_eq!(cfg.refresh_rate().as_amount(), 1);
    }
}
