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
//! # Lifecycle
//!
//! 1. **Build**: Create components and services via build config
//! 2. **Run**: Spawn services as background tasks
//!
//! # Example
//!
//! ```ignore
//! use vertex_node_api::{Protocol, NodeContext};
//! use vertex_swarm_api::SwarmProtocol;
//!
//! // Build protocol using config that implements a build config trait
//! let built = SwarmProtocol::<MyLightConfig>::build(config, &ctx).await?;
//!
//! // Run the protocol (services are consumed)
//! let components = built.run(ctx.executor());
//! ```

use async_trait::async_trait;
use core::marker::PhantomData;
use vertex_node_api::{Built, NodeContext, Protocol};
use vertex_tasks::TaskExecutor;

use crate::{RunnableClientService, RunnableNode, SwarmBuildConfig, SwarmServices};

/// Swarm protocol marker type.
///
/// This is a unified protocol type that works for all capability levels.
/// The build config `Cfg` determines what components and services are built.
pub struct SwarmProtocol<Cfg>(PhantomData<Cfg>);

impl<Cfg> Default for SwarmProtocol<Cfg> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

#[async_trait]
impl<Cfg> Protocol for SwarmProtocol<Cfg>
where
    Cfg: SwarmBuildConfig,
{
    type Config = Cfg;
    type Components = Cfg::Components;
    type Services = SwarmServices<Cfg::Types>;
    type BuildError = Cfg::Error;

    async fn build(
        config: Self::Config,
        ctx: &NodeContext,
    ) -> Result<Built<Self>, Self::BuildError> {
        let (components, services) = config.build(ctx).await?;
        Ok(Built::new(components, services))
    }

    fn run(services: Self::Services, executor: &TaskExecutor) {
        let (node, client_service, _client_handle) = services.into_parts();

        executor.spawn_critical("swarm_client_service", async move {
            if let Err(e) = RunnableClientService::run(client_service).await {
                tracing::error!(error = %e, "SwarmClientService error");
            }
        });

        executor.spawn_critical("swarm_node", async move {
            if let Err(e) = RunnableNode::run(node).await {
                tracing::error!(error = %e, "SwarmNode error");
            }
        });
    }
}
