//! Type-state node builder for Vertex.

use std::net::{IpAddr, SocketAddr};
use std::path::Path;

use vertex_node_api::{InfrastructureContext, NodeBuildsProtocol, NodeProtocol, NodeRpcConfig};
use vertex_node_core::dirs::DataDirs;
use vertex_rpc_server::{GrpcRegistry, RegistersGrpcServices};
use vertex_tasks::TaskExecutor;

use crate::{InfrastructureError, LaunchError, NodeHandle};

/// Context for launching a node with executor, directories, and API config.
#[derive(Clone)]
pub struct LaunchContext<A = ()> {
    pub executor: TaskExecutor,
    pub dirs: DataDirs,
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
}

impl<A: Send + Sync> InfrastructureContext for LaunchContext<A> {
    fn executor(&self) -> &TaskExecutor {
        &self.executor
    }

    fn data_dir(&self) -> &Path {
        &self.dirs.network
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

    /// Launch the node with gRPC server for protocol services.
    pub async fn launch(self) -> Result<NodeHandle<P::Components>, LaunchError<P::BuildError>>
    where
        P::Components: RegistersGrpcServices,
    {
        use tracing::info;

        // Infrastructure configuration
        info!("Data directory: {}", self.ctx.dirs.root.display());
        info!("gRPC address: {}", self.ctx.grpc_addr());

        let grpc_addr = self.ctx.grpc_addr();

        // Launch the protocol (builds components and spawns services)
        let components = P::launch(self.config, &self.ctx)
            .await
            .map_err(LaunchError::Protocol)?;

        // Create gRPC registry and let components register their services
        let mut registry = GrpcRegistry::new();
        components.register_grpc_services(&mut registry);

        // Convert registry to server and spawn as critical task
        let grpc_handle = registry
            .into_server(grpc_addr)
            .map_err(InfrastructureError::GrpcReflection)?;

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
