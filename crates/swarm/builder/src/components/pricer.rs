//! Pricer builder trait and implementations.

use vertex_bandwidth_core::{FixedPricer, Pricer};
use vertex_swarm_api::{LightTypes, NetworkConfig};

use crate::SwarmBuilderContext;

/// Builds the pricer component.
pub trait PricerBuilder<Types: LightTypes, Cfg: NetworkConfig>: Send + Sync + 'static {
    /// The pricer type produced.
    type Pricer: Pricer + Clone + Send + Sync + 'static;

    /// Build the pricer given the context.
    fn build_pricer(self, ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Pricer;
}

/// Default fixed pricer builder.
#[derive(Debug, Clone, Default)]
pub struct FixedPricerBuilder {
    base_price: Option<u64>,
}

impl FixedPricerBuilder {
    /// Create with custom base price.
    pub fn with_base_price(base_price: u64) -> Self {
        Self {
            base_price: Some(base_price),
        }
    }
}

impl<Types: LightTypes, Cfg: NetworkConfig> PricerBuilder<Types, Cfg> for FixedPricerBuilder {
    type Pricer = FixedPricer;

    fn build_pricer(self, _ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Pricer {
        match self.base_price {
            Some(price) => FixedPricer::new(price),
            None => FixedPricer::default(),
        }
    }
}
