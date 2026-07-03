//! Validated bandwidth accounting configuration.

use vertex_swarm_accounting_pricing::FixedPricingConfig;
use vertex_swarm_api::{Au, SwarmAccountingConfig, SwarmPricingConfig};

#[cfg(feature = "cli")]
use crate::args::AccountingArgs;
use crate::constants::*;

/// Bandwidth accounting configuration.
///
/// Generic over the pricing configuration type `P`. Use [`DefaultAccountingConfig`]
/// for the standard CLI-produced configuration with fixed pricing.
#[derive(Debug, Clone)]
pub struct AccountingConfig<P = FixedPricingConfig> {
    payment_threshold: u64,
    /// The unscaled threshold; [`for_client`](Self::for_client) scales only the
    /// working values, so the creditor serve lines keep deriving from the base.
    base_payment_threshold: u64,
    payment_tolerance_percent: u64,
    refresh_rate: u64,
    early_payment_percent: u64,
    client_only_factor: u64,
    pricing: P,
}

/// Default bandwidth config using fixed pricing (CLI-produced).
pub type DefaultAccountingConfig = AccountingConfig<FixedPricingConfig>;

impl<P> AccountingConfig<P> {
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
            base_payment_threshold: payment_threshold,
            payment_tolerance_percent,
            refresh_rate,
            early_payment_percent,
            client_only_factor,
            pricing,
        }
    }

    /// This config scaled to the line a storer enforces on a client:
    /// `payment_threshold` and `refresh_rate` divided by `client_only_factor`,
    /// floored at one. Pacing against the unscaled storer figures would let a
    /// burst cross the storer's disconnect line before our settle engages.
    /// Debtor direction only: `base_payment_threshold` is preserved so the
    /// creditor serve lines stay keyed on the remote's type at full scale.
    pub fn for_client(self) -> Self {
        let factor = self.client_only_factor.max(1);
        Self {
            payment_threshold: (self.payment_threshold / factor).max(1),
            refresh_rate: (self.refresh_rate / factor).max(1),
            ..self
        }
    }
}

#[cfg(feature = "cli")]
impl From<&AccountingArgs> for AccountingConfig<FixedPricingConfig> {
    fn from(args: &AccountingArgs) -> Self {
        Self {
            payment_threshold: args.payment_threshold,
            base_payment_threshold: args.payment_threshold,
            payment_tolerance_percent: args.payment_tolerance_percent,
            refresh_rate: args.refresh_rate,
            early_payment_percent: args.early_payment_percent,
            client_only_factor: args.client_only_factor,
            pricing: FixedPricingConfig::from(&args.pricing),
        }
    }
}

impl Default for AccountingConfig<FixedPricingConfig> {
    fn default() -> Self {
        Self {
            payment_threshold: DEFAULT_PAYMENT_THRESHOLD,
            base_payment_threshold: DEFAULT_PAYMENT_THRESHOLD,
            payment_tolerance_percent: DEFAULT_PAYMENT_TOLERANCE_PERCENT,
            refresh_rate: DEFAULT_REFRESH_RATE,
            early_payment_percent: DEFAULT_EARLY_PAYMENT_PERCENT,
            client_only_factor: DEFAULT_CLIENT_ONLY_FACTOR,
            pricing: FixedPricingConfig::default(),
        }
    }
}

impl<P> SwarmAccountingConfig for AccountingConfig<P>
where
    P: Send + Sync,
{
    fn payment_threshold(&self) -> Au {
        Au::from_amount(self.payment_threshold)
    }

    fn base_payment_threshold(&self) -> Au {
        Au::from_amount(self.base_payment_threshold)
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

impl<P> SwarmPricingConfig for AccountingConfig<P>
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

    #[cfg(feature = "cli")]
    #[test]
    fn from_args_carries_the_thresholds() {
        let config = AccountingConfig::from(&AccountingArgs::default());
        assert_eq!(
            config.payment_threshold().as_amount(),
            DEFAULT_PAYMENT_THRESHOLD
        );
        assert_eq!(config.refresh_rate().as_amount(), DEFAULT_REFRESH_RATE);
        assert_eq!(config.client_only_factor(), DEFAULT_CLIENT_ONLY_FACTOR);
    }

    #[test]
    fn for_client_scales_threshold_and_refresh_by_the_factor() {
        let storer = DefaultAccountingConfig::default();
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
        // The unscaled base survives the scaling: creditor serve lines derive
        // from it, so a client node still extends full-scale lines.
        assert_eq!(
            client.base_payment_threshold().as_amount(),
            storer_threshold
        );
        assert_eq!(
            client.client_payment_threshold().as_amount(),
            storer_threshold / factor
        );
        // The disconnect threshold derives from the now-scaled payment threshold,
        // so it scales down with it: we pace against the same client ceiling the
        // serving storer enforces on us.
        assert!(client.disconnect_threshold() < storer_disconnect);
        assert_eq!(client.payment_tolerance_percent(), storer_tolerance);
        assert_eq!(client.client_only_factor(), factor);
    }

    #[test]
    fn for_client_floors_at_one() {
        let cfg = AccountingConfig {
            payment_threshold: 5,
            refresh_rate: 5,
            client_only_factor: 1000,
            ..DefaultAccountingConfig::default()
        }
        .for_client();
        assert_eq!(cfg.payment_threshold().as_amount(), 1);
        assert_eq!(cfg.refresh_rate().as_amount(), 1);
    }
}
