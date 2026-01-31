//! Pricer builder trait and implementations.

use vertex_bandwidth_core::{FixedPricer, NoPricer, Pricer};
use vertex_swarm_api::{SwarmAccountingConfig, DefaultAccountingConfig, SwarmIdentity, SwarmClientTypes, SwarmNetworkConfig};

use crate::SwarmBuilderContext;

/// Builds the pricer component.
pub trait PricerBuilder<Types: SwarmClientTypes, Cfg: SwarmNetworkConfig>: Send + Sync + 'static {
    /// The pricer type produced.
    type Pricer: Pricer + Clone + Send + Sync + 'static;

    /// Build the pricer given the context.
    fn build_pricer(self, ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Pricer;
}

/// No-op pricer for bootnodes (no pricing protocol participation).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoPricerBuilder;

impl<Types: SwarmClientTypes, Cfg: SwarmNetworkConfig> PricerBuilder<Types, Cfg> for NoPricerBuilder {
    type Pricer = NoPricer;

    fn build_pricer(self, _ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Pricer {
        NoPricer
    }
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

impl<Types: SwarmClientTypes, Cfg: SwarmNetworkConfig> PricerBuilder<Types, Cfg> for FixedPricerBuilder {
    type Pricer = FixedPricer;

    fn build_pricer(self, ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Pricer {
        let spec = ctx.identity.spec();
        let base_price = self.base_price.unwrap_or_else(|| DefaultAccountingConfig.base_price());
        FixedPricer::new(base_price, spec)
    }
}
