//! Pricer builder.

use vertex_swarm_bandwidth_pricing::{DefaultPricingConfig, FixedPricer, NoPricer, Pricer};
use vertex_swarm_api::{SwarmIdentity, SwarmClientTypes, SwarmNetworkConfig, SwarmPricingConfig};

use crate::SwarmBuilderContext;

/// Builder for chunk pricing components.
pub trait PricerBuilder<Types: SwarmClientTypes, Cfg: SwarmNetworkConfig>: Send + Sync + 'static {
    type Pricer: Pricer + Clone + Send + Sync + 'static;

    fn build_pricer(self, ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Pricer;
}

/// No-op pricer for bootnodes.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoPricerBuilder;

impl<Types: SwarmClientTypes, Cfg: SwarmNetworkConfig> PricerBuilder<Types, Cfg> for NoPricerBuilder {
    type Pricer = NoPricer;

    fn build_pricer(self, _ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Pricer {
        NoPricer
    }
}

/// Fixed-price pricer builder.
#[derive(Debug, Clone, Default)]
pub struct FixedPricerBuilder<C: SwarmPricingConfig + Clone = DefaultPricingConfig> {
    config: C,
}

impl<C: SwarmPricingConfig + Clone> FixedPricerBuilder<C> {
    pub fn new(config: C) -> Self {
        Self { config }
    }
}

impl<Types: SwarmClientTypes, Cfg: SwarmNetworkConfig, C: SwarmPricingConfig + Clone + 'static>
    PricerBuilder<Types, Cfg> for FixedPricerBuilder<C>
{
    type Pricer = FixedPricer;

    fn build_pricer(self, ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Pricer {
        let spec = ctx.identity.spec();
        FixedPricer::new(self.config.base_price(), spec)
    }
}
