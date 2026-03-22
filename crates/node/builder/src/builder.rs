//! Type-state node builder for Vertex.

use std::net::SocketAddr;
use std::path::Path;

use vertex_node_api::{InfrastructureContext, NodeBuildsProtocol, NodeProtocol};
use vertex_node_core::dirs::DataDirs;
use vertex_rpc_server::{GrpcRegistry, RegistersGrpcServices};
use vertex_tasks::TaskExecutor;

use crate::{InfrastructureError, LaunchError, NodeHandle};

/// Context for launching a node with executor and directories.
#[derive(Clone)]
pub struct LaunchContext {
    pub executor: TaskExecutor,
    pub dirs: DataDirs,
}

impl LaunchContext {
    /// Create a new launch context.
    pub fn new(executor: TaskExecutor, dirs: DataDirs) -> Self {
        Self { executor, dirs }
    }

    /// Get the data directory root.
    pub fn data_dir(&self) -> &std::path::PathBuf {
        &self.dirs.root
    }
}

impl InfrastructureContext for LaunchContext {
    fn executor(&self) -> &TaskExecutor {
        &self.executor
    }

    fn data_dir(&self) -> &Path {
        &self.dirs.network
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

    /// Add launch context (executor and data directories).
    #[must_use]
    pub fn with_launch_context(self, executor: TaskExecutor, dirs: DataDirs) -> WithLaunchContext {
        WithLaunchContext {
            ctx: LaunchContext::new(executor, dirs),
        }
    }
}

impl Default for NodeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder with launch context attached.
pub struct WithLaunchContext {
    ctx: LaunchContext,
}

impl WithLaunchContext {
    /// Create from an existing launch context.
    pub fn new(ctx: LaunchContext) -> Self {
        Self { ctx }
    }

    /// Get a reference to the launch context.
    pub fn context(&self) -> &LaunchContext {
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
    pub fn with_protocol<C: NodeBuildsProtocol>(self, config: C) -> WithProtocol<C::Protocol> {
        tracing::info!("Protocol: {}", config.protocol_name());
        WithProtocol {
            ctx: self.ctx,
            config,
        }
    }
}

/// Builder with protocol configuration, ready to launch.
pub struct WithProtocol<P: NodeProtocol> {
    ctx: LaunchContext,
    config: P::Config,
}

impl<P: NodeProtocol> WithProtocol<P>
where
    P::Config: NodeBuildsProtocol,
{
    /// Get a reference to the launch context.
    pub fn context(&self) -> &LaunchContext {
        &self.ctx
    }

    /// Launch the node with gRPC server for protocol services.
    pub async fn launch(
        self,
        grpc_addr: SocketAddr,
    ) -> Result<NodeHandle<P::Components>, LaunchError<P::BuildError>>
    where
        P::Components: RegistersGrpcServices,
    {
        use tracing::info;

        // Infrastructure configuration
        info!("Data directory: {}", self.ctx.dirs.root.display());
        info!("gRPC address: {}", grpc_addr);

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

        self.ctx
            .executor
            .spawn_critical_with_graceful_shutdown_signal(
                "grpc.server",
                move |shutdown| async move {
                    if let Err(e) = grpc_handle
                        .serve_with_shutdown(shutdown.ignore_guard())
                        .await
                    {
                        tracing::error!(error = %e, "gRPC server error");
                    }
                },
            );

        Ok(NodeHandle::new(
            components,
            self.ctx.executor.on_shutdown_signal().clone(),
        ))
    }

    /// Launch the node without gRPC server.
    ///
    /// Use this when you don't need the gRPC API (e.g. embedded use).
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
