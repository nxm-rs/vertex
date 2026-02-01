//! Swarm protocol implementation for node infrastructure integration.
//!
//! Provides [`SwarmProtocol`] which implements [`vertex_node_api::NodeProtocol`].
//! A single type works for all capability levels (Bootnode, Client, Storer).

use core::marker::PhantomData;
use vertex_node_api::{NodeContext, NodeProtocol};
use vertex_tasks::TaskExecutor;

use crate::SwarmLaunchConfig;

/// Swarm protocol marker type.
///
/// The launch config `Cfg` determines what providers and task are built.
pub struct SwarmProtocol<Cfg>(PhantomData<Cfg>);

impl<Cfg> Default for SwarmProtocol<Cfg> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

#[async_trait::async_trait]
impl<Cfg> NodeProtocol for SwarmProtocol<Cfg>
where
    Cfg: SwarmLaunchConfig,
{
    type Config = Cfg;
    type Components = Cfg::Providers;
    type BuildError = Cfg::Error;

    async fn launch(
        config: Self::Config,
        ctx: &NodeContext,
        executor: &TaskExecutor,
    ) -> Result<Self::Components, Self::BuildError> {
        let (task, providers) = config.build(ctx).await?;

        // Spawn the node's main event loop
        executor.spawn_critical("swarm", task);

        Ok(providers)
    }
}
