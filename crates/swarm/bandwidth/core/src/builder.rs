//! Bandwidth accounting builder.

use std::sync::Arc;

use vertex_swarm_api::{SwarmAccountingConfig, SwarmIdentity, SwarmSettlementProvider, SwarmSpec};

use crate::store::AccountingStore;
use crate::{Accounting, ClientAccounting};

/// Builder for bandwidth accounting.
///
/// Constructs [`ClientAccounting`] which combines per-peer balance tracking
/// with chunk pricing. Settlement providers are added via [`with_settlement`].
///
/// # Example
///
/// ```ignore
/// let accounting = AccountingBuilder::new(bandwidth_config)
///     .with_settlement(PseudosettleProvider::new(&config))
///     .build(spec, &identity);
/// ```
pub struct AccountingBuilder<C> {
    config: C,
    providers: Vec<Box<dyn SwarmSettlementProvider>>,
    store: Option<Arc<dyn AccountingStore>>,
}

impl<C: SwarmAccountingConfig> AccountingBuilder<C> {
    /// Create a new accounting builder.
    pub fn new(config: C) -> Self {
        Self {
            config,
            providers: Vec::new(),
            store: None,
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

impl<C: SwarmAccountingConfig + Clone + 'static> AccountingBuilder<C> {
    /// Build the accounting system.
    ///
    /// If a store was configured via [`with_store`](AccountingBuilder::with_store),
    /// persisted peer state is loaded into the in-memory cache before returning.
    pub fn build<I, S>(
        self,
        spec: Arc<S>,
        identity: &I,
    ) -> ClientAccounting<Arc<Accounting<C, I>>, S>
    where
        I: SwarmIdentity + Clone,
        S: SwarmSpec,
    {
        let mut accounting =
            Accounting::with_providers(self.config, identity.clone(), self.providers);

        if let Some(store) = self.store {
            accounting.set_store(store);
            if let Err(e) = accounting.load_all_from_store() {
                tracing::warn!(error = %e, "Failed to load accounting state from store");
            }
        }

        ClientAccounting::new(Arc::new(accounting), spec)
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
    use crate::BandwidthConfig;
    use vertex_swarm_test_utils::test_identity_arc as test_identity;

    #[test]
    fn test_builder_basic() {
        let identity = test_identity();
        let config = BandwidthConfig::default();

        let _accounting = AccountingBuilder::new(config).build(identity.spec().clone(), &identity);
    }

    #[test]
    fn test_builder_apply() {
        let identity = test_identity();
        let config = BandwidthConfig::default();

        let _accounting = AccountingBuilder::new(config)
            .apply_if(true, |b| b)
            .build(identity.spec().clone(), &identity);
    }
}
