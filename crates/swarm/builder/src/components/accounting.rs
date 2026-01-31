//! Accounting builder trait and implementations.

use std::sync::Arc;

use vertex_bandwidth_core::Accounting;
use vertex_bandwidth_pseudosettle::PseudosettleProvider;
use vertex_bandwidth_swap::SwapProvider;
use vertex_swarm_api::{
    SwarmAccountingConfig, SwarmBandwidthAccounting, BandwidthMode, DefaultAccountingConfig, SwarmIdentity,
    SwarmClientTypes, SwarmNetworkConfig, NoBandwidthIncentives,
};

use crate::SwarmBuilderContext;

/// Builds the accounting component.
pub trait AccountingBuilder<Types: SwarmClientTypes, Cfg: SwarmNetworkConfig>: Send + Sync + 'static {
    /// The accounting type produced.
    type Accounting: SwarmBandwidthAccounting + Send + Sync + 'static;

    /// Build the accounting given the context.
    fn build_accounting(self, ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Accounting;
}

/// No-op accounting builder (for bootnodes without bandwidth incentives).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoAccountingBuilder;

impl<Types: SwarmClientTypes, Cfg: SwarmNetworkConfig> AccountingBuilder<Types, Cfg> for NoAccountingBuilder {
    type Accounting = NoBandwidthIncentives<Arc<Types::Identity>>;

    fn build_accounting(self, ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Accounting {
        NoBandwidthIncentives::new(Arc::clone(&ctx.identity))
    }
}

/// Pseudosettle-only accounting builder.
#[derive(Debug, Clone, Default)]
pub struct PseudosettleAccountingBuilder<C: SwarmAccountingConfig + Clone = DefaultAccountingConfig> {
    config: C,
}

impl<C: SwarmAccountingConfig + Clone + 'static> PseudosettleAccountingBuilder<C> {
    pub fn with_config(config: C) -> Self {
        Self { config }
    }
}

impl<Types: SwarmClientTypes, Cfg: SwarmNetworkConfig, C: SwarmAccountingConfig + Clone + 'static>
    AccountingBuilder<Types, Cfg> for PseudosettleAccountingBuilder<C>
{
    type Accounting = Arc<Accounting<C, Arc<Types::Identity>>>;

    fn build_accounting(self, ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Accounting {
        Arc::new(Accounting::with_providers(
            self.config.clone(),
            Arc::clone(&ctx.identity),
            vec![Box::new(PseudosettleProvider::new(self.config))],
        ))
    }
}

/// SWAP-only accounting builder.
#[derive(Debug, Clone, Default)]
pub struct SwapAccountingBuilder<C: SwarmAccountingConfig + Clone = DefaultAccountingConfig> {
    config: C,
}

impl<C: SwarmAccountingConfig + Clone + 'static> SwapAccountingBuilder<C> {
    pub fn with_config(config: C) -> Self {
        Self { config }
    }
}

impl<Types: SwarmClientTypes, Cfg: SwarmNetworkConfig, C: SwarmAccountingConfig + Clone + 'static>
    AccountingBuilder<Types, Cfg> for SwapAccountingBuilder<C>
{
    type Accounting = Arc<Accounting<C, Arc<Types::Identity>>>;

    fn build_accounting(self, ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Accounting {
        Arc::new(Accounting::with_providers(
            self.config.clone(),
            Arc::clone(&ctx.identity),
            vec![Box::new(SwapProvider::new(self.config))],
        ))
    }
}

/// Combined pseudosettle + SWAP accounting builder.
///
/// Pseudosettle runs first to refresh allowance, then SWAP settles if still over threshold.
#[derive(Debug, Clone, Default)]
pub struct CombinedAccountingBuilder<C: SwarmAccountingConfig + Clone = DefaultAccountingConfig> {
    config: C,
}

impl<C: SwarmAccountingConfig + Clone + 'static> CombinedAccountingBuilder<C> {
    pub fn with_config(config: C) -> Self {
        Self { config }
    }
}

impl<Types: SwarmClientTypes, Cfg: SwarmNetworkConfig, C: SwarmAccountingConfig + Clone + 'static>
    AccountingBuilder<Types, Cfg> for CombinedAccountingBuilder<C>
{
    type Accounting = Arc<Accounting<C, Arc<Types::Identity>>>;

    fn build_accounting(self, ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Accounting {
        Arc::new(Accounting::with_providers(
            self.config.clone(),
            Arc::clone(&ctx.identity),
            vec![
                Box::new(PseudosettleProvider::new(self.config.clone())),
                Box::new(SwapProvider::new(self.config)),
            ],
        ))
    }
}

/// Mode-based accounting builder.
///
/// Selects the appropriate accounting based on `BandwidthMode` in the config.
#[derive(Debug, Clone, Default)]
pub struct ModeBasedAccountingBuilder<C: SwarmAccountingConfig + Clone = DefaultAccountingConfig> {
    config: C,
}

impl<C: SwarmAccountingConfig + Clone + 'static> ModeBasedAccountingBuilder<C> {
    pub fn with_config(config: C) -> Self {
        Self { config }
    }

    pub fn build_for_mode<I: SwarmIdentity>(self, identity: I) -> ModeBasedAccounting<C, I> {
        match self.config.mode() {
            BandwidthMode::None => ModeBasedAccounting::None(NoBandwidthIncentives::new(identity)),
            BandwidthMode::Pseudosettle => {
                ModeBasedAccounting::Enabled(Arc::new(Accounting::with_providers(
                    self.config.clone(),
                    identity,
                    vec![Box::new(PseudosettleProvider::new(self.config))],
                )))
            }
            BandwidthMode::Swap => {
                ModeBasedAccounting::Enabled(Arc::new(Accounting::with_providers(
                    self.config.clone(),
                    identity,
                    vec![Box::new(SwapProvider::new(self.config))],
                )))
            }
            BandwidthMode::Both => {
                ModeBasedAccounting::Enabled(Arc::new(Accounting::with_providers(
                    self.config.clone(),
                    identity,
                    vec![
                        Box::new(PseudosettleProvider::new(self.config.clone())),
                        Box::new(SwapProvider::new(self.config)),
                    ],
                )))
            }
        }
    }
}

/// Accounting type selected based on `BandwidthMode`.
pub enum ModeBasedAccounting<C: SwarmAccountingConfig + Clone + 'static, I: SwarmIdentity> {
    None(NoBandwidthIncentives<I>),
    Enabled(Arc<Accounting<C, I>>),
}
