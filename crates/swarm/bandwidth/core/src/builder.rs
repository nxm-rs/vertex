//! Bandwidth accounting builder with integrated pricing.

use std::sync::Arc;

use vertex_swarm_api::{
    SwarmAccountingConfig, SwarmIdentity, SwarmPricing, SwarmPricingBuilder, SwarmPricingConfig,
    SwarmSettlementProvider, SwarmSpec,
};
use vertex_swarm_bandwidth_pricing::NoPricer;

use crate::store::AccountingStore;
use crate::{Accounting, ClientAccounting};

/// Builder for bandwidth accounting with integrated pricing.
///
/// Constructs [`ClientAccounting`] which combines per-peer balance tracking
/// with chunk pricing. Settlement providers are added via [`with_settlement`].
///
/// # Example
///
/// ```ignore
/// let accounting = AccountingBuilder::new(bandwidth_config)
///     .with_pricer_from_config(spec.clone())
///     .with_settlement(PseudosettleProvider::new(&config))
///     .build(&identity);
/// ```
pub struct AccountingBuilder<C, P = NoPricer> {
    config: C,
    pricing: P,
    providers: Vec<Box<dyn SwarmSettlementProvider>>,
    store: Option<Arc<dyn AccountingStore>>,
}

impl<C: SwarmAccountingConfig> AccountingBuilder<C, NoPricer> {
    /// Create a new accounting builder with no pricer.
    pub fn new(config: C) -> Self {
        Self {
            config,
            pricing: NoPricer,
            providers: Vec::new(),
            store: None,
        }
    }
}

impl<C, P> AccountingBuilder<C, P> {
    /// Set the pricing strategy.
    pub fn with_pricing<NewP: SwarmPricing + Clone + Send + Sync + 'static>(
        self,
        pricing: NewP,
    ) -> AccountingBuilder<C, NewP> {
        AccountingBuilder {
            config: self.config,
            pricing,
            providers: self.providers,
            store: self.store,
        }
    }

    /// Set the persistence store for accounting state.
    pub fn with_store(mut self, store: Arc<dyn AccountingStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Add a settlement provider.
    ///
    /// Multiple providers can be added. They are called in order during settlement.
    /// For `BandwidthMode::Both`, add pseudosettle before swap.
    pub fn with_settlement(mut self, provider: impl SwarmSettlementProvider + 'static) -> Self {
        self.providers.push(Box::new(provider));
        self
    }

    /// Add multiple settlement providers.
    pub fn with_settlements(
        mut self,
        providers: impl IntoIterator<Item = Box<dyn SwarmSettlementProvider>>,
    ) -> Self {
        self.providers.extend(providers);
        self
    }

    /// Apply a transformation function.
    pub fn apply<F>(self, f: F) -> Self
    where
        F: FnOnce(Self) -> Self,
    {
        f(self)
    }

    /// Apply a transformation function if condition is true.
    pub fn apply_if<F>(self, cond: bool, f: F) -> Self
    where
        F: FnOnce(Self) -> Self,
    {
        if cond { f(self) } else { self }
    }

    /// Get a reference to the config.
    pub fn config(&self) -> &C {
        &self.config
    }
}

impl<C> AccountingBuilder<C, NoPricer>
where
    C: SwarmPricingConfig,
{
    /// Build pricer from config's embedded pricing configuration.
    pub fn with_pricer_from_config<S>(
        self,
        spec: Arc<S>,
    ) -> AccountingBuilder<C, <C::Pricing as SwarmPricingBuilder<S>>::Pricer>
    where
        C::Pricing: SwarmPricingBuilder<S>,
        S: SwarmSpec + Send + Sync + 'static,
    {
        let pricer = self.config.pricing().build_pricer(spec);
        self.with_pricing(pricer)
    }
}

impl<C: SwarmAccountingConfig + Clone + 'static, P: SwarmPricing + Clone + Send + Sync + 'static>
    AccountingBuilder<C, P>
{
    /// Build the accounting system.
    ///
    /// If a store was configured via [`with_store`](AccountingBuilder::with_store),
    /// persisted peer state is loaded into the in-memory cache before returning.
    pub fn build<I: SwarmIdentity + Clone>(
        self,
        identity: &I,
    ) -> ClientAccounting<Arc<Accounting<C, I>>, P> {
        let mut accounting = Accounting::with_providers(
            self.config,
            identity.clone(),
            self.providers,
        );

        if let Some(store) = self.store {
            accounting.set_store(store);
            if let Err(e) = accounting.load_all_from_store() {
                tracing::warn!(error = %e, "Failed to load accounting state from store");
            }
        }

        ClientAccounting::new(Arc::new(accounting), self.pricing)
    }
}

/// No-op accounting builder for bootnodes.
///
/// Always allows transfers without balance tracking.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoAccountingBuilder;

impl NoAccountingBuilder {
    /// Create a new no-op accounting builder.
    pub fn new() -> Self {
        Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DefaultBandwidthConfig;
    use vertex_swarm_test_utils::test_identity_arc as test_identity;

    #[test]
    fn test_builder_with_pricer_from_config() {
        let identity = test_identity();
        let config = DefaultBandwidthConfig::default();

        let _accounting = AccountingBuilder::new(config)
            .with_pricer_from_config(identity.spec().clone())
            .build(&identity);
    }

    #[test]
    fn test_builder_with_custom_pricing() {
        use vertex_swarm_bandwidth_pricing::FixedPricer;

        let identity = test_identity();
        let config = DefaultBandwidthConfig::default();
        let pricer = FixedPricer::new(5000, identity.spec().clone());

        let _accounting = AccountingBuilder::new(config)
            .with_pricing(pricer)
            .build(&identity);
    }

    #[test]
    fn test_builder_apply() {
        let identity = test_identity();
        let config = DefaultBandwidthConfig::default();

        let _accounting = AccountingBuilder::new(config)
            .with_pricer_from_config(identity.spec().clone())
            .apply_if(true, |b| b)
            .build(&identity);
    }
}
