//! The Swarm `NodeProtocol` the node builder launches.
//!
//! Lives here, not in `vertex-swarm-api`, because `serve_view` names the gRPC
//! adapter ([`GrpcAdapter`]): the orphan-rule escape hatch that carries the
//! `RegistersGrpcServices` impls for the component containers. `Components` stays
//! the bare container; only the serve view is wrapped.

use core::marker::PhantomData;

use vertex_node_api::{InfrastructureContext, NodeProtocol};
use vertex_swarm_api::SwarmLaunchConfig;
use vertex_swarm_rpc::GrpcAdapter;

/// Swarm protocol marker type.
///
/// The launch config `Cfg` determines what providers and task are built.
pub struct SwarmProtocol<Cfg>(PhantomData<Cfg>);

impl<Cfg> Default for SwarmProtocol<Cfg> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<Cfg> NodeProtocol for SwarmProtocol<Cfg>
where
    Cfg: SwarmLaunchConfig,
{
    type Config = Cfg;
    type Components = Cfg::Providers;
    type ServeView = GrpcAdapter<Cfg::Providers>;
    type BuildError = Cfg::Error;

    async fn launch(
        config: Self::Config,
        ctx: &dyn InfrastructureContext,
    ) -> Result<Self::Components, Self::BuildError> {
        let (task_fn, providers) = config.build(ctx).await?;

        // Spawn the node's main event loop with graceful shutdown support. The run
        // loop returns only after shutdown was signalled, or on an unexpected early
        // exit; in either case request graceful shutdown so the rest of the node
        // tears down rather than lingering without its event loop.
        let executor = ctx.executor().clone();
        ctx.executor().spawn_critical_with_graceful_shutdown_signal(
            "swarm.protocol",
            move |shutdown| {
                let task = task_fn(shutdown);
                async move {
                    task.await;
                    let _ = executor.initiate_graceful_shutdown();
                }
            },
        );

        Ok(providers)
    }

    fn serve_view(components: &Self::Components) -> Self::ServeView {
        GrpcAdapter::new(components.clone())
    }
}
