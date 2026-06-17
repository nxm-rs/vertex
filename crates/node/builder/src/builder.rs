//! Type-state node builder for Vertex.

use std::net::{IpAddr, SocketAddr};
use std::path::Path;

use vertex_node_api::{InfrastructureContext, NodeBuildsProtocol, NodeProtocol, NodeRpcConfig};
use vertex_node_core::args::DatabaseConfig;
use vertex_node_core::dirs::DataDirs;
use vertex_rpc_server::{GrpcTransport, ServeWith, Transport, TransportServer};
use vertex_tasks::TaskExecutor;

use crate::{InfrastructureError, LaunchError, NodeHandle};

/// Context for launching a node with executor, directories, and API config.
#[derive(Clone)]
pub struct LaunchContext<A = ()> {
    pub executor: TaskExecutor,
    pub dirs: DataDirs,
    pub api: A,
    pub database: DatabaseConfig,
}

impl<A> LaunchContext<A> {
    /// Create a new launch context with an in-memory database configuration.
    pub fn new(executor: TaskExecutor, dirs: DataDirs, api: A) -> Self {
        Self {
            executor,
            dirs,
            api,
            database: DatabaseConfig::default(),
        }
    }

    /// Set the resolved database configuration.
    #[must_use]
    pub fn with_database_config(mut self, database: DatabaseConfig) -> Self {
        self.database = database;
        self
    }

    /// Get the data directory root.
    pub fn data_dir(&self) -> &std::path::PathBuf {
        &self.dirs.root
    }
}

impl<A: Send + Sync> InfrastructureContext for LaunchContext<A> {
    fn executor(&self) -> &TaskExecutor {
        &self.executor
    }

    fn data_dir(&self) -> &Path {
        &self.dirs.network
    }

    fn db_path(&self) -> Option<&Path> {
        self.database.path.as_deref()
    }
}

impl<A: NodeRpcConfig> LaunchContext<A> {
    /// Get the gRPC socket address from configuration.
    pub fn grpc_addr(&self) -> SocketAddr {
        let ip: IpAddr = self.api.grpc_addr().parse().unwrap_or_else(|_| {
            tracing::warn!(
                addr = %self.api.grpc_addr(),
                "Invalid gRPC address, falling back to localhost"
            );
            [127, 0, 0, 1].into()
        });
        SocketAddr::new(ip, self.api.grpc_port())
    }
}

/// Node builder - first stage for adding launch context.
pub struct NodeBuilder;

impl NodeBuilder {
    /// Create a new node builder.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Add launch context (executor, data directories, and API config).
    #[must_use]
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
pub struct WithLaunchContext<A> {
    ctx: LaunchContext<A>,
}

impl<A> WithLaunchContext<A> {
    /// Get a reference to the launch context.
    pub fn context(&self) -> &LaunchContext<A> {
        &self.ctx
    }

    /// Set the resolved database configuration on the launch context.
    #[must_use]
    pub fn with_database_config(mut self, database: DatabaseConfig) -> Self {
        self.ctx = self.ctx.with_database_config(database);
        self
    }

    /// Get the data directories.
    pub fn dirs(&self) -> &DataDirs {
        &self.ctx.dirs
    }

    /// Get the task executor.
    pub fn executor(&self) -> &TaskExecutor {
        &self.ctx.executor
    }

    /// Provide the protocol configuration (protocol type inferred from config).
    #[must_use]
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

impl<P: NodeProtocol, A: NodeRpcConfig + Send + Sync> WithProtocol<P, A>
where
    P::Config: NodeBuildsProtocol,
{
    /// Get a reference to the launch context.
    pub fn context(&self) -> &LaunchContext<A> {
        &self.ctx
    }

    /// Launch the node, serving its components with the chosen transport.
    ///
    /// The transport is selected by the caller; this path names no transport.
    pub async fn launch_with<Tr: Transport>(
        self,
    ) -> Result<NodeHandle<P::Components>, LaunchError<P::BuildError>>
    where
        P::Components: ServeWith<Tr>,
    {
        use tracing::info;

        // Infrastructure configuration
        info!("Data directory: {}", self.ctx.dirs.root.display());
        info!("RPC address: {}", self.ctx.grpc_addr());

        let addr = self.ctx.grpc_addr();

        // Launch the protocol (builds components and spawns services)
        let components = P::launch(self.config, &self.ctx)
            .await
            .map_err(LaunchError::Protocol)?;

        // Populate the transport registry from the components.
        let mut registry = Tr::Registry::default();
        components.register(&mut registry);

        // Bind the server and spawn it as a critical task.
        let server = Tr::into_server(registry, addr)
            .map_err(|e| InfrastructureError::Transport(e.into()))?;

        self.ctx
            .executor
            .spawn_critical_with_graceful_shutdown_signal(
                "rpc.server",
                move |shutdown| async move {
                    if let Err(e) = server.serve_with_shutdown(shutdown.ignore_guard()).await {
                        tracing::error!(error = %e, "RPC server error");
                    }
                },
            );

        Ok(NodeHandle::new(
            components,
            self.ctx.executor.on_shutdown_signal().clone(),
        ))
    }

    /// Launch the node with the gRPC transport (back-compat alias).
    pub async fn launch(self) -> Result<NodeHandle<P::Components>, LaunchError<P::BuildError>>
    where
        P::Components: ServeWith<GrpcTransport>,
    {
        self.launch_with::<GrpcTransport>().await
    }
}

impl<P: NodeProtocol> WithProtocol<P, ()>
where
    P::Config: NodeBuildsProtocol,
{
    /// Launch the node without gRPC server.
    ///
    /// Use this when you don't need the gRPC API.
    pub async fn launch_without_grpc(
        self,
    ) -> Result<NodeHandle<P::Components>, LaunchError<P::BuildError>> {
        use tracing::info;

        // Infrastructure configuration
        info!("Data directory: {}", self.ctx.dirs.root.display());
        info!("gRPC: disabled");

        // Launch the protocol (builds components and spawns services)
        let components = P::launch(self.config, &self.ctx)
            .await
            .map_err(LaunchError::Protocol)?;

        Ok(NodeHandle::new(
            components,
            self.ctx.executor.on_shutdown_signal().clone(),
        ))
    }
}
