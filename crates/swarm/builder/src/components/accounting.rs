//! Accounting builder trait and implementations.

use std::sync::Arc;

use vertex_bandwidth_core::{Accounting, AccountingConfig};
use vertex_swarm_api::{BandwidthAccounting, LightTypes, NetworkConfig, NoBandwidthIncentives};

use crate::SwarmBuilderContext;

/// Builds the accounting component.
pub trait AccountingBuilder<Types: LightTypes, Cfg: NetworkConfig>: Send + Sync + 'static {
    /// The accounting type produced.
    type Accounting: BandwidthAccounting + Send + Sync + 'static;

    /// Build the accounting given the context.
    fn build_accounting(self, ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Accounting;
}

/// Default bandwidth accounting builder.
///
/// Produces `Arc<Accounting>` which implements `BandwidthAccounting`.
#[derive(Debug, Clone, Default)]
pub struct BandwidthAccountingBuilder {
    config: AccountingConfig,
}

impl BandwidthAccountingBuilder {
    /// Create with custom config.
    pub fn with_config(config: AccountingConfig) -> Self {
        Self { config }
    }
}

impl<Types: LightTypes, Cfg: NetworkConfig> AccountingBuilder<Types, Cfg>
    for BandwidthAccountingBuilder
{
    type Accounting = Arc<Accounting>;

    fn build_accounting(self, _ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Accounting {
        Arc::new(Accounting::new(self.config))
    }
}

/// No-op accounting builder (for nodes without bandwidth incentives).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoAccountingBuilder;

impl<Types: LightTypes, Cfg: NetworkConfig> AccountingBuilder<Types, Cfg> for NoAccountingBuilder {
    type Accounting = NoBandwidthIncentives;

    fn build_accounting(self, _ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Accounting {
        NoBandwidthIncentives
    }
}
