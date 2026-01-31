//! Swarm protocol implementation for node infrastructure integration.
//!
//! Provides [`SwarmProtocol`] which implements [`vertex_node_api::NodeProtocol`].
//! A single type works for all capability levels (Bootnode, Client, Storer).

use core::marker::PhantomData;
use vertex_node_api::{NodeContext, NodeProtocol};
use vertex_tasks::TaskExecutor;

use crate::SwarmLaunchConfig;
use vertex_tasks::SpawnableTask;

/// Swarm protocol marker type.
///
/// The launch config `Cfg` determines what components and services are built.
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
    type Components = Cfg::Components;
    type BuildError = Cfg::Error;

    async fn launch(
        config: Self::Config,
        ctx: &NodeContext,
        executor: &TaskExecutor,
    ) -> Result<Self::Components, Self::BuildError> {
        let (components, services) = config.build(ctx).await?;

        // Spawn services as critical tasks
        let (node, client_service, _client_handle) = services.into_parts();
        executor.spawn_critical("swarm_client_service", client_service.into_task());
        executor.spawn_critical("swarm_node", node.into_task());

        Ok(components)
    }
}
