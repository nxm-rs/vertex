//! Shared build helpers for node construction.

use std::future::Future;
use std::sync::Arc;

use vertex_tasks::NodeTaskFn;
use vertex_swarm_bandwidth::{AccountingBuilder, ClientAccounting};
use vertex_swarm_spec::Spec;
use vertex_tasks::GracefulShutdown;

/// Build bandwidth accounting from configuration.
pub(crate) fn build_accounting<A>(
    spec: Arc<Spec>,
    identity: &Arc<vertex_swarm_identity::Identity>,
    config: A,
) -> ClientAccounting<
    Arc<vertex_swarm_bandwidth::Accounting<A, Arc<vertex_swarm_identity::Identity>>>,
    <A::Pricing as vertex_swarm_api::SwarmPricingBuilder<Spec>>::Pricer,
>
where
    A: vertex_swarm_api::SwarmAccountingConfig + vertex_swarm_api::SwarmPricingConfig + Clone + 'static,
    A::Pricing: vertex_swarm_api::SwarmPricingBuilder<Spec>,
{
    AccountingBuilder::new(config)
        .with_pricer_from_config(spec)
        .build(identity)
}

/// Wrap a future factory as a NodeTaskFn with graceful shutdown support.
pub(crate) fn single_task<F, Fut>(f: F) -> NodeTaskFn
where
    F: FnOnce(GracefulShutdown) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    Box::new(move |shutdown| Box::pin(f(shutdown)))
}
