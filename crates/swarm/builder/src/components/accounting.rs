//! Bandwidth accounting builder.

use std::sync::Arc;

use vertex_swarm_bandwidth::{Accounting, DefaultAccountingConfig, NoAccounting};
use vertex_swarm_bandwidth_pseudosettle::PseudosettleProvider;
use vertex_swarm_bandwidth_swap::SwapProvider;
use vertex_swarm_api::{
    BandwidthMode, SwarmAccountingConfig, SwarmBandwidthAccounting, SwarmClientTypes,
    SwarmNetworkConfig, SwarmSettlementProvider,
};

use crate::SwarmBuilderContext;

/// Builder for bandwidth accounting components.
pub trait AccountingBuilder<Types: SwarmClientTypes, Cfg: SwarmNetworkConfig>: Send + Sync + 'static {
    type Accounting: SwarmBandwidthAccounting + Send + Sync + 'static;

    fn build_accounting(self, ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Accounting;
}

/// No-op accounting for bootnodes (always allows transfers, no balance tracking).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoAccountingBuilder;

impl<Types: SwarmClientTypes, Cfg: SwarmNetworkConfig> AccountingBuilder<Types, Cfg> for NoAccountingBuilder {
    type Accounting = NoAccounting<Arc<Types::Identity>>;

    fn build_accounting(self, ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Accounting {
        NoAccounting::new(Arc::clone(&ctx.identity))
    }
}

/// Accounting builder that creates settlement providers based on [`BandwidthMode`].
///
/// Use `Arc<YourConfig>` as the type parameter for cheap cloning.
#[derive(Debug, Clone, Default)]
pub struct DefaultAccountingBuilder<C: SwarmAccountingConfig + Clone = DefaultAccountingConfig> {
    config: C,
}

impl<C: SwarmAccountingConfig + Clone + 'static> DefaultAccountingBuilder<C> {
    pub fn new(config: C) -> Self {
        Self { config }
    }

    fn build_providers(&self) -> Vec<Box<dyn SwarmSettlementProvider>> {
        match self.config.mode() {
            BandwidthMode::None => vec![],
            BandwidthMode::Pseudosettle => {
                vec![Box::new(PseudosettleProvider::new(self.config.clone()))]
            }
            BandwidthMode::Swap => {
                vec![Box::new(SwapProvider::new(self.config.clone()))]
            }
            BandwidthMode::Both => {
                vec![
                    Box::new(PseudosettleProvider::new(self.config.clone())),
                    Box::new(SwapProvider::new(self.config.clone())),
                ]
            }
        }
    }
}

impl<Types: SwarmClientTypes, Cfg: SwarmNetworkConfig, C: SwarmAccountingConfig + Clone + 'static>
    AccountingBuilder<Types, Cfg> for DefaultAccountingBuilder<C>
{
    type Accounting = Arc<Accounting<C, Arc<Types::Identity>>>;

    fn build_accounting(self, ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Accounting {
        Arc::new(Accounting::with_providers(
            self.config.clone(),
            Arc::clone(&ctx.identity),
            self.build_providers(),
        ))
    }
}
