//! Protocol trait for node infrastructure integration.
//!
//! This module defines the [`Protocol`] trait which provides the lifecycle
//! interface between a network protocol (like Swarm) and the node infrastructure.
//!
//! # Lifecycle
//!
//! 1. **Build**: Create components and services from config + infrastructure
//! 2. **Run**: Start services using the task executor
//!
//! # Example
//!
//! ```ignore
//! use vertex_node_api::{Protocol, NodeContext};
//!
//! // Build protocol from configuration
//! let built = SwarmLightProtocol::build(config, &ctx).await?;
//!
//! // Run the protocol (services are consumed, components remain)
//! let components = built.run(ctx.executor());
//! ```

use crate::NodeContext;
use async_trait::async_trait;
use vertex_tasks::TaskExecutor;

/// A build configuration that knows which protocol it builds.
///
/// This trait enables type inference at `with_protocol()` - the config type
/// uniquely determines the protocol type.
///
/// # Example
///
/// ```ignore
/// use vertex_node_api::BuildsProtocol;
/// use vertex_swarm_api::SwarmLightProtocol;
///
/// impl BuildsProtocol for MyLightBuildConfig {
///     type Protocol = SwarmLightProtocol<Self>;
///
///     fn protocol_name(&self) -> &'static str {
///         "Swarm"
///     }
///
///     fn node_type_name(&self) -> &'static str {
///         "Light"
///     }
/// }
/// ```
pub trait BuildsProtocol: Send + Sync + 'static {
    /// The protocol this config builds.
    type Protocol: Protocol<Config = Self>;

    /// Human-readable protocol name for logging (e.g., "Swarm", "Ethereum").
    fn protocol_name(&self) -> &'static str {
        "Unknown"
    }
}

/// A network protocol that can be built and run by node infrastructure.
///
/// # Components vs Services
///
/// - **Components**: Static data for RPC queries (identity, topology, accounting).
///   Remains available after `run()` is called.
/// - **Services**: Runnable tasks (SwarmNode, ClientService). Consumed by `run()` -
///   moved into spawned tasks.
///
/// # Example
///
/// ```ignore
/// use vertex_node_api::{Protocol, Built, NodeContext};
/// use vertex_tasks::TaskExecutor;
///
/// struct MyProtocol;
///
/// #[async_trait]
/// impl Protocol for MyProtocol {
///     type Config = MyConfig;
///     type Components = MyComponents;
///     type Services = MyServices;
///     type BuildError = MyError;
///
///     async fn build(
///         config: Self::Config,
///         ctx: &NodeContext,
///     ) -> Result<Built<Self>, Self::BuildError> {
///         // Build components and services...
///         Ok(Built::new(components, services))
///     }
///
///     fn run(services: Self::Services, executor: &TaskExecutor) {
///         // Spawn background tasks
///     }
/// }
/// ```
#[async_trait]
pub trait Protocol: Sized + Send + Sync + 'static {
    /// Protocol-specific configuration.
    type Config: Send + Sync + 'static;

    /// Static components for queries and RPC (identity, topology, accounting).
    ///
    /// Remains available after `run()` is called.
    type Components: Send + Sync + 'static;

    /// Runnable services (SwarmNode, ClientService).
    ///
    /// Consumed by `run()` - moved into spawned tasks.
    type Services: Send + 'static;

    /// Error type for build failures.
    type BuildError: std::error::Error + Send + Sync + 'static;

    /// Build protocol from configuration using node infrastructure.
    ///
    /// Returns [`Built<Self>`] containing both components and services.
    async fn build(
        config: Self::Config,
        ctx: &NodeContext,
    ) -> Result<Built<Self>, Self::BuildError>;

    /// Run the protocol services.
    ///
    /// Spawns background tasks via the executor. Services are moved into
    /// the spawned tasks and cannot be recovered.
    fn run(services: Self::Services, executor: &TaskExecutor);
}

/// Result of building a protocol.
///
/// Contains both static components and runnable services.
/// Call [`run()`](Self::run) to start the protocol.
///
/// # Example
///
/// ```ignore
/// // Build returns both components and services
/// let built = SwarmLightProtocol::build(config, &ctx).await?;
///
/// // Run consumes services, returns components
/// let components = built.run(ctx.executor());
///
/// // Components remain available for the lifetime of the node
/// println!("Overlay: {}", components.identity.overlay_address());
/// ```
pub struct Built<P: Protocol> {
    /// Static components - remain available after run().
    pub components: P::Components,
    /// Runnable services - consumed by run().
    pub services: P::Services,
}

impl<P: Protocol> Built<P> {
    /// Create a new Built with the given components and services.
    pub fn new(components: P::Components, services: P::Services) -> Self {
        Self {
            components,
            services,
        }
    }

    /// Run the protocol, returning components for continued use.
    ///
    /// Services are moved into spawned tasks. Components remain
    /// available for queries and RPC.
    pub fn run(self, executor: &TaskExecutor) -> P::Components {
        P::run(self.services, executor);
        self.components
    }
}
