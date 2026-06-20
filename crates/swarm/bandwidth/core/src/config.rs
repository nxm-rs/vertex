//! Validated bandwidth accounting configuration.

use vertex_swarm_api::{
    Au, BandwidthMode, SwarmAccountingConfig, SwarmNodeType, SwarmPricingConfig,
};
use vertex_swarm_bandwidth_pricing::FixedPricingConfig;

use crate::args::BandwidthArgs;
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
    #[error("throttle-allowance-percent must be in 1..=100")]
    ThrottleAllowancePercentOutOfRange,
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
    throttle_allowance_percent: u8,
    pricing: P,
}

/// Default bandwidth config using fixed pricing (CLI-produced).
pub type DefaultBandwidthConfig = BandwidthConfig<FixedPricingConfig>;

impl<P> BandwidthConfig<P> {
    /// Create with explicit values.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mode: BandwidthMode,
        payment_threshold: u64,
        payment_tolerance_percent: u64,
        refresh_rate: u64,
        early_payment_percent: u64,
        client_only_factor: u64,
        throttle_allowance_percent: u8,
        pricing: P,
    ) -> Self {
        Self {
            mode,
            payment_threshold,
            payment_tolerance_percent,
            refresh_rate,
            early_payment_percent,
            client_only_factor,
            throttle_allowance_percent,
            pricing,
        }
    }

    /// Apply an operator mode override; `None` keeps the seeded node-type default.
    pub fn with_mode_override(mut self, override_mode: Option<BandwidthMode>) -> Self {
        if let Some(mode) = override_mode {
            self.mode = mode;
        }
        self
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

impl BandwidthConfig<FixedPricingConfig> {
    /// Mode seeded from the node type via [`BandwidthMode::default_for`], every
    /// other field at its [`Default`]. Fresh-defaults only: operator-tuned fields
    /// flow through [`TryFrom<(SwarmNodeType, &BandwidthArgs)>`](BandwidthConfig#impl-TryFrom).
    pub fn for_node_type(node_type: SwarmNodeType) -> Self {
        Self {
            mode: BandwidthMode::default_for(node_type),
            ..Self::default()
        }
    }
}

impl TryFrom<(SwarmNodeType, &BandwidthArgs)> for BandwidthConfig<FixedPricingConfig> {
    type Error = BandwidthConfigError;

    /// Validated config, mode seeded from `node_type` unless `--bandwidth.mode`
    /// is set. Cross-field validation runs against the effective mode.
    fn try_from((node_type, args): (SwarmNodeType, &BandwidthArgs)) -> Result<Self, Self::Error> {
        let default = BandwidthArgs::default();
        let mode = args.effective_mode(node_type);

        match mode {
            BandwidthMode::None => {
                let has_non_default = args.refresh_rate != default.refresh_rate
                    || args.payment_threshold != default.payment_threshold
                    || args.payment_tolerance_percent != default.payment_tolerance_percent
                    || args.early_payment_percent != default.early_payment_percent
                    || args.client_only_factor != default.client_only_factor;
                if has_non_default {
                    return Err(BandwidthConfigError::OptionsWithDisabledMode);
                }
            }
            BandwidthMode::Pseudosettle => {
                if args.early_payment_percent != default.early_payment_percent {
                    return Err(BandwidthConfigError::EarlyPercentWithoutSwap);
                }
            }
            BandwidthMode::Swap => {
                if args.refresh_rate != default.refresh_rate {
                    return Err(BandwidthConfigError::RefreshRateWithoutPseudosettle);
                }
            }
            BandwidthMode::Both => {}
        }

        if !(1..=100).contains(&args.throttle_allowance_percent) {
            return Err(BandwidthConfigError::ThrottleAllowancePercentOutOfRange);
        }

        Ok(Self {
            mode,
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
            // Pinned to pseudosettle, not the enum's `#[default]`, so the bare
            // `Default` path is stable as node-type defaults evolve.
            mode: BandwidthMode::Pseudosettle,
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
    fn mode(&self) -> BandwidthMode {
        self.mode
    }

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
    use crate::args::BandwidthModeArg;

    #[test]
    fn default_throttle_allowance_percent() {
        let config = DefaultBandwidthConfig::default();
        assert_eq!(
            config.throttle_allowance_percent(),
            DEFAULT_THROTTLE_ALLOWANCE_PERCENT
        );
    }

    #[test]
    fn for_node_type_seeds_the_mode() {
        assert_eq!(
            DefaultBandwidthConfig::for_node_type(SwarmNodeType::Bootnode).mode(),
            BandwidthMode::None
        );
        assert_eq!(
            DefaultBandwidthConfig::for_node_type(SwarmNodeType::Client).mode(),
            BandwidthMode::Pseudosettle
        );
        assert_eq!(
            DefaultBandwidthConfig::for_node_type(SwarmNodeType::Storer).mode(),
            BandwidthMode::Both
        );
    }

    #[test]
    fn mode_override_replaces_only_when_some() {
        // None keeps the node-type default (a storer stays on Both).
        let derived =
            DefaultBandwidthConfig::for_node_type(SwarmNodeType::Storer).with_mode_override(None);
        assert_eq!(derived.mode(), BandwidthMode::Both);

        // Some replaces it: explicit Pseudosettle on a storer is not upgraded to Both.
        let overridden = DefaultBandwidthConfig::for_node_type(SwarmNodeType::Storer)
            .with_mode_override(Some(BandwidthMode::Pseudosettle));
        assert_eq!(overridden.mode(), BandwidthMode::Pseudosettle);
    }

    #[test]
    fn default_mode_is_pinned_to_pseudosettle() {
        // A bare default always pseudosettles, independent of the enum's #[default].
        assert_eq!(
            DefaultBandwidthConfig::default().mode(),
            BandwidthMode::Pseudosettle
        );
    }

    #[test]
    fn try_from_seeds_mode_from_node_type() {
        for (node_type, expected) in [
            (SwarmNodeType::Bootnode, BandwidthMode::None),
            (SwarmNodeType::Client, BandwidthMode::Pseudosettle),
            (SwarmNodeType::Storer, BandwidthMode::Both),
        ] {
            let args = BandwidthArgs::default();
            let config =
                BandwidthConfig::try_from((node_type, &args)).expect("default args validate");
            assert_eq!(config.mode(), expected, "node type {node_type:?}");
        }
    }

    #[test]
    fn try_from_honors_explicit_mode_override() {
        // Explicit Pseudosettle on a storer is not upgraded to Both.
        let args = BandwidthArgs {
            mode: Some(BandwidthModeArg::Pseudosettle),
            ..BandwidthArgs::default()
        };
        let config =
            BandwidthConfig::try_from((SwarmNodeType::Storer, &args)).expect("valid override");
        assert_eq!(config.mode(), BandwidthMode::Pseudosettle);
    }

    #[test]
    fn try_from_validates_against_effective_mode() {
        // A storer derives Both, so a non-default refresh-rate is accepted.
        let args = BandwidthArgs {
            refresh_rate: DEFAULT_REFRESH_RATE + 1,
            ..BandwidthArgs::default()
        };
        assert!(BandwidthConfig::try_from((SwarmNodeType::Storer, &args)).is_ok());

        // An explicit swap override forbids a non-default refresh-rate.
        let args = BandwidthArgs {
            mode: Some(BandwidthModeArg::Swap),
            refresh_rate: DEFAULT_REFRESH_RATE + 1,
            ..BandwidthArgs::default()
        };
        assert!(matches!(
            BandwidthConfig::try_from((SwarmNodeType::Storer, &args)),
            Err(BandwidthConfigError::RefreshRateWithoutPseudosettle)
        ));

        // A bootnode derives None, so any non-default tuning is rejected.
        let args = BandwidthArgs {
            payment_threshold: DEFAULT_PAYMENT_THRESHOLD + 1,
            ..BandwidthArgs::default()
        };
        assert!(matches!(
            BandwidthConfig::try_from((SwarmNodeType::Bootnode, &args)),
            Err(BandwidthConfigError::OptionsWithDisabledMode)
        ));
    }

    #[test]
    fn throttle_allowance_percent_in_range_is_accepted() {
        for pct in [1u8, 50, 85, 100] {
            let args = BandwidthArgs {
                throttle_allowance_percent: pct,
                ..BandwidthArgs::default()
            };
            let config =
                BandwidthConfig::try_from((SwarmNodeType::Client, &args)).expect("valid percent");
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
                BandwidthConfig::try_from((SwarmNodeType::Client, &args)),
                Err(BandwidthConfigError::ThrottleAllowancePercentOutOfRange)
            ));
        }
    }
}
