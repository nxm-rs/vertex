//! Validated pricing configuration.

use std::sync::Arc;

use vertex_swarm_api::SwarmPricingBuilder;
use vertex_swarm_spec::SwarmSpec;

use crate::constants::DEFAULT_BASE_PRICE;
use crate::FixedPricer;

/// Validated fixed-rate chunk pricing configuration.
#[derive(Debug, Clone, Copy)]
pub struct FixedPricingConfig {
    base_price: u64,
}

impl FixedPricingConfig {
    /// Create with explicit base price.
    pub const fn new(base_price: u64) -> Self {
        Self { base_price }
    }
}

impl Default for FixedPricingConfig {
    fn default() -> Self {
        Self {
            base_price: DEFAULT_BASE_PRICE,
        }
    }
}

impl<S: SwarmSpec + Send + Sync + 'static> SwarmPricingBuilder<S> for FixedPricingConfig {
    type Pricer = FixedPricer<S>;

    fn build_pricer(&self, spec: Arc<S>) -> Self::Pricer {
        FixedPricer::new(self.base_price, spec)
    }
}

#[cfg(feature = "cli")]
impl From<&crate::args::FixedPricingArgs> for FixedPricingConfig {
    fn from(args: &crate::args::FixedPricingArgs) -> Self {
        Self {
            base_price: args.base_price,
        }
    }
}
