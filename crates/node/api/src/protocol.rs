//! Protocol lifecycle trait for node infrastructure integration.

use async_trait::async_trait;

use crate::InfrastructureContext;

/// A build configuration that knows which protocol it builds.
///
/// This trait enables type inference at `with_protocol()` - the config type
/// uniquely determines the protocol type.
///
/// # Example
///
/// ```ignore
/// use vertex_node_api::NodeBuildsProtocol;
/// use vertex_swarm_api::SwarmProtocol;
///
/// impl NodeBuildsProtocol for MyLightBuildConfig {
///     type Protocol = SwarmProtocol<Self>;
///
///     fn protocol_name(&self) -> &'static str {
///         "Swarm"
///     }
/// }
/// ```
pub trait NodeBuildsProtocol: Send + Sync + 'static {
    /// The protocol this config builds.
    type Protocol: NodeProtocol<Config = Self>;

    /// Human-readable protocol name for logging (e.g., "Swarm", "Ethereum").
    fn protocol_name(&self) -> &'static str {
        "Unknown"
    }
}

/// A network protocol that can be launched by node infrastructure.
///
/// # Components
///
/// Components are static data for RPC queries (identity, topology, accounting).
/// They remain available after `launch()` returns.
///
/// Services (background tasks like SwarmNode, ClientService) are spawned
/// internally by `launch()` and don't appear in the trait signature.
///
/// # Example
///
/// ```ignore
/// use vertex_node_api::{NodeProtocol, InfrastructureContext};
///
/// struct MyProtocol;
///
/// #[async_trait]
/// impl NodeProtocol for MyProtocol {
///     type Config = MyConfig;
///     type Components = MyComponents;
///     type BuildError = MyError;
///
///     async fn launch(
///         config: Self::Config,
///         ctx: &dyn InfrastructureContext,
///     ) -> Result<Self::Components, Self::BuildError> {
///         // Build components using ctx.data_dir() for persistence
///         let components = build_components(&config, ctx.data_dir())?;
///
///         // Spawn services as background tasks
///         let services = build_services(&config)?;
///         ctx.executor().spawn_critical("my_service", services.run());
///
///         Ok(components)
///     }
/// }
/// ```
#[async_trait]
pub trait NodeProtocol: Sized + Send + Sync + 'static {
    /// Protocol-specific configuration.
    type Config: Send + Sync + 'static;

    /// Static components for queries and RPC (identity, topology, accounting).
    ///
    /// Remains available after `launch()` returns.
    type Components: Send + Sync + 'static;

    /// Error type for launch failures.
    type BuildError: std::error::Error + Send + Sync + 'static;

    /// Build and launch the protocol.
    ///
    /// This method:
    /// 1. Builds components from the configuration and context
    /// 2. Spawns background services via the context's executor
    /// 3. Returns components for continued use (RPC, metrics, etc.)
    ///
    /// Services are spawned as critical tasks - if they fail, the node shuts down.
    async fn launch(
        config: Self::Config,
        ctx: &dyn InfrastructureContext,
    ) -> Result<Self::Components, Self::BuildError>;
}
