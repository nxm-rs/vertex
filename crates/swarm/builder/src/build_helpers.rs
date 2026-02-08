//! Shared build helpers for node construction.

use std::future::Future;
use std::sync::Arc;

use vertex_swarm_api::NodeTask;
use vertex_swarm_bandwidth::{AccountingBuilder, ClientAccounting};
use vertex_swarm_spec::Spec;

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

/// Wrap a single future as a NodeTask.
pub(crate) fn single_task<F>(fut: F) -> NodeTask
where
    F: Future<Output = ()> + Send + 'static,
{
    Box::pin(fut)
}

/// Wrap two concurrent futures as a NodeTask using tokio::select!.
pub(crate) fn dual_task<F1, F2>(label1: &'static str, fut1: F1, label2: &'static str, fut2: F2) -> NodeTask
where
    F1: Future<Output = ()> + Send + 'static,
    F2: Future<Output = ()> + Send + 'static,
{
    Box::pin(async move {
        tokio::select! {
            () = fut1 => tracing::info!("{} completed", label1),
            () = fut2 => tracing::info!("{} completed", label2),
        }
    })
}
