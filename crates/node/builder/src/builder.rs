//! Type-state node builder for Vertex.

use std::net::{IpAddr, SocketAddr};
use std::path::Path;

use vertex_node_api::{InfrastructureContext, NodeBuildsProtocol, NodeProtocol, NodeRpcConfig};
use vertex_node_core::args::DatabaseConfig;
use vertex_node_core::dirs::DataDirs;
use vertex_rpc_server::{GrpcTransport, ServeWith, Transport, TransportServer};
use vertex_tasks::TaskExecutor;

use crate::{InfrastructureError, LaunchError, NodeHandle};

/// Executor, directories, and API config needed to launch a node.
#[derive(Clone)]
pub struct LaunchContext<A = ()> {
    pub executor: TaskExecutor,
    pub dirs: DataDirs,
    pub api: A,
    pub database: DatabaseConfig,
}

impl<A> LaunchContext<A> {
    /// Defaults to an in-memory database configuration.
    pub fn new(executor: TaskExecutor, dirs: DataDirs, api: A) -> Self {
        Self {
            executor,
            dirs,
            api,
            database: DatabaseConfig::default(),
        }
    }

    #[must_use]
    pub fn with_database_config(mut self, database: DatabaseConfig) -> Self {
        self.database = database;
        self
    }

    /// Data directory root.
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
    /// gRPC socket address; falls back to localhost if the configured address is unparseable.
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
    #[must_use]
    pub fn new() -> Self {
        Self
    }

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
    pub fn context(&self) -> &LaunchContext<A> {
        &self.ctx
    }

    #[must_use]
    pub fn with_database_config(mut self, database: DatabaseConfig) -> Self {
        self.ctx = self.ctx.with_database_config(database);
        self
    }

    pub fn dirs(&self) -> &DataDirs {
        &self.ctx.dirs
    }

    pub fn executor(&self) -> &TaskExecutor {
        &self.ctx.executor
    }

    /// Protocol type is inferred from the config.
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
    pub fn context(&self) -> &LaunchContext<A> {
        &self.ctx
    }

    /// Launch the node, serving its components with the caller-selected transport.
    pub async fn launch_with<Tr: Transport>(
        self,
    ) -> Result<NodeHandle<P::Components>, LaunchError<P::BuildError>>
    where
        P::Components: ServeWith<Tr>,
    {
        use tracing::info;

        info!("Data directory: {}", self.ctx.dirs.root.display());
        info!("RPC address: {}", self.ctx.grpc_addr());

        let addr = self.ctx.grpc_addr();

        let components = P::launch(self.config, &self.ctx)
            .await
            .map_err(LaunchError::Protocol)?;

        let mut registry = Tr::Registry::default();
        components.register(&mut registry);

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

    /// Launch with the gRPC transport.
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
    /// Launch without a gRPC server.
    pub async fn launch_without_grpc(
        self,
    ) -> Result<NodeHandle<P::Components>, LaunchError<P::BuildError>> {
        use tracing::info;

        info!("Data directory: {}", self.ctx.dirs.root.display());
        info!("gRPC: disabled");

        let components = P::launch(self.config, &self.ctx)
            .await
            .map_err(LaunchError::Protocol)?;

        Ok(NodeHandle::new(
            components,
            self.ctx.executor.on_shutdown_signal().clone(),
        ))
    }
}
