//! Swarm protocol implementation for node infrastructure integration.
//!
//! This module provides the [`SwarmProtocol`] type which implements
//! [`vertex_node_api::Protocol`], enabling Swarm to be run by node infrastructure.
//!
//! # Unified Protocol
//!
//! A single `SwarmProtocol<Cfg>` works for all capability levels (Light, Publisher, Full).
//! The build config determines which components are created, but all nodes run the
//! same services (node event loop + client service).
//!
//! # Example
//!
//! ```ignore
//! use vertex_node_api::Protocol;
//! use vertex_swarm_api::SwarmProtocol;
//!
//! // Launch builds and spawns in one step
//! let components = SwarmProtocol::<MyLightConfig>::launch(config, &ctx, &executor).await?;
//! ```

use async_trait::async_trait;
use core::marker::PhantomData;
use vertex_node_api::{NodeContext, Protocol};
use vertex_tasks::TaskExecutor;

use crate::{SpawnableTask, SwarmLaunchConfig};

/// Swarm protocol marker type.
///
/// This is a unified protocol type that works for all capability levels.
/// The launch config `Cfg` determines what components and services are built.
pub struct SwarmProtocol<Cfg>(PhantomData<Cfg>);

impl<Cfg> Default for SwarmProtocol<Cfg> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

#[async_trait]
impl<Cfg> Protocol for SwarmProtocol<Cfg>
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
        executor.spawn_critical("swarm_client_service", client_service.spawn_task());
        executor.spawn_critical("swarm_node", node.spawn_task());

        Ok(components)
    }
}
