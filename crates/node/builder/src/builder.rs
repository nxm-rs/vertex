//! Node builder type-state pattern.
//!
//! The builder follows a type-state pattern where each stage is a distinct type:
//!
//! ```text
//! NodeBuilder
//!   │
//!   ├── with_launch_context(executor, dirs, api)
//!   ▼
//! WithLaunchContext<A>
//!   │
//!   ├── with_protocol(config: impl NodeProtocolConfig)
//!   ▼
//! WithProtocol<P, A>
//!   │
//!   ├── launch()
//!   ▼
//! NodeHandle<P::Components>
//! ```
//!
//! # Example
//!
//! ```ignore
//! use vertex_node_builder::NodeBuilder;
//!
//! // Protocol type is inferred from config!
//! let handle = NodeBuilder::new()
//!     .with_launch_context(executor, dirs, api_config)
//!     .with_protocol(my_light_config)
//!     .launch()
//!     .await?;
//!
//! handle.wait_for_exit().await?;
//! ```

use std::net::{IpAddr, SocketAddr};

use vertex_node_api::{NodeBuildsProtocol, NodeContext, NodeProtocol, NodeRpcConfig};
use vertex_node_core::dirs::DataDirs;
use vertex_rpc_server::{GrpcRegistry, RegistersGrpcServices};
use vertex_tasks::TaskExecutor;

use crate::NodeHandle;

/// Context for launching a node.
///
/// Contains all infrastructure needed to launch any node:
/// - Task executor for spawning background tasks
/// - Data directories for persistent storage
/// - API configuration (gRPC, metrics)
#[derive(Clone)]
pub struct LaunchContext<A = ()> {
    /// Task executor for spawning background tasks.
    pub executor: TaskExecutor,
    /// Data directories for this node.
    pub dirs: DataDirs,
    /// API configuration.
    pub api: A,
}

impl<A> LaunchContext<A> {
    /// Create a new launch context.
    pub fn new(executor: TaskExecutor, dirs: DataDirs, api: A) -> Self {
        Self {
            executor,
            dirs,
            api,
        }
    }

    /// Get the data directory root.
    pub fn data_dir(&self) -> &std::path::PathBuf {
        &self.dirs.root
    }

    /// Create a node context from this launch context.
    ///
    /// The node context uses the network-specific directory as the data path.
    pub fn node_context(&self) -> NodeContext {
        NodeContext::new(self.executor.clone(), self.dirs.network.clone())
    }
}

impl<A: NodeRpcConfig> LaunchContext<A> {
    /// Get the gRPC socket address from configuration.
    pub fn grpc_addr(&self) -> SocketAddr {
        let ip: IpAddr = self
            .api
            .grpc_addr()
            .parse()
            .unwrap_or([127, 0, 0, 1].into());
        SocketAddr::new(ip, self.api.grpc_port())
    }
}

/// Node builder - first stage.
///
/// Use [`with_launch_context`](Self::with_launch_context) to provide
/// infrastructure configuration, then [`with_protocol`](WithLaunchContext::with_protocol)
/// to specify what protocol to run.
pub struct NodeBuilder;

impl NodeBuilder {
    /// Create a new node builder.
    pub fn new() -> Self {
        Self
    }

    /// Add launch context (executor, data directories, and API config).
    pub fn with_launch_context<A>(
        self,
        executor: TaskExecutor,
        dirs: DataDirs,
        api: A,
    ) -> WithLaunchContext<A> {
        WithLaunchContext {
            ctx: LaunchContext::new(executor, dirs, api),
        }
    }
}

impl Default for NodeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder with launch context attached.
///
/// Use [`with_protocol`](Self::with_protocol) to provide the protocol configuration.
pub struct WithLaunchContext<A> {
    ctx: LaunchContext<A>,
}

impl<A> WithLaunchContext<A> {
    /// Get a reference to the launch context.
    pub fn context(&self) -> &LaunchContext<A> {
        &self.ctx
    }

    /// Get the data directories.
    pub fn dirs(&self) -> &DataDirs {
        &self.ctx.dirs
    }

    /// Get the task executor.
    pub fn executor(&self) -> &TaskExecutor {
        &self.ctx.executor
    }

    /// Provide the protocol configuration.
    ///
    /// The protocol type is inferred from the config type via [`NodeBuildsProtocol`].
    pub fn with_protocol<C: NodeBuildsProtocol>(self, config: C) -> WithProtocol<C::Protocol, A> {
        tracing::info!("Protocol: {}", config.protocol_name());
        WithProtocol {
            ctx: self.ctx,
            config,
        }
    }
}

/// Builder with protocol configuration, ready to launch.
pub struct WithProtocol<P: NodeProtocol, A> {
    ctx: LaunchContext<A>,
    config: P::Config,
}

impl<P: NodeProtocol, A: NodeRpcConfig> WithProtocol<P, A>
where
    P::Config: NodeBuildsProtocol,
{
    /// Get a reference to the launch context.
    pub fn context(&self) -> &LaunchContext<A> {
        &self.ctx
    }

    /// Launch the node.
    ///
    /// This builds the protocol components, spawns services, and starts the gRPC server.
    /// The gRPC address is taken from the launch context's API configuration.
    ///
    /// Components must implement [`RegistersGrpcServices`] to register their
    /// protocol-specific RPC services.
    pub async fn launch(self) -> Result<NodeHandle<P::Components>, P::BuildError>
    where
        P::Components: RegistersGrpcServices,
    {
        use tracing::info;

        // Infrastructure configuration
        info!("Data directory: {}", self.ctx.dirs.root.display());
        info!("gRPC address: {}", self.ctx.grpc_addr());

        let node_ctx = self.ctx.node_context();
        let grpc_addr = self.ctx.grpc_addr();

        // Launch the protocol (builds components and spawns services)
        let components = P::launch(self.config, &node_ctx, &self.ctx.executor).await?;

        // Create gRPC registry and let components register their services
        let mut registry = GrpcRegistry::new();
        components.register_grpc_services(&mut registry);

        // Convert registry to server and spawn as critical task
        let grpc_handle = registry
            .into_server(grpc_addr)
            .expect("failed to build gRPC server");

        let shutdown = self.ctx.executor.on_shutdown_signal().clone();
        self.ctx.executor.spawn_critical("grpc_server", async move {
            if let Err(e) = grpc_handle.serve_with_shutdown(shutdown).await {
                tracing::error!(error = %e, "gRPC server error");
            }
        });

        Ok(NodeHandle::new(
            components,
            self.ctx.executor.on_shutdown_signal().clone(),
        ))
    }
}

impl<P: NodeProtocol> WithProtocol<P, ()>
where
    P::Config: NodeBuildsProtocol,
{
    /// Launch the node without gRPC server.
    ///
    /// Use this when you don't need the gRPC API.
    pub async fn launch_without_grpc(self) -> Result<NodeHandle<P::Components>, P::BuildError> {
        use tracing::info;

        // Infrastructure configuration
        info!("Data directory: {}", self.ctx.dirs.root.display());
        info!("gRPC: disabled");

        let node_ctx = self.ctx.node_context();

        // Launch the protocol (builds components and spawns services)
        let components = P::launch(self.config, &node_ctx, &self.ctx.executor).await?;

        Ok(NodeHandle::new(
            components,
            self.ctx.executor.on_shutdown_signal().clone(),
        ))
    }
}
