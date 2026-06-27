//! Type-state node builder for Vertex.

use std::net::{IpAddr, SocketAddr};
use std::path::Path;

use vertex_node_api::{InfrastructureContext, NodeBuildsProtocol, NodeProtocol, NodeRpcConfig};
use vertex_node_core::args::DatabaseConfig;
use vertex_node_core::dirs::DataDirs;
use vertex_rpc_server::{GrpcTransport, ServeWith, Transport, TransportServer};
use vertex_tasks::TaskExecutor;

use crate::{InfrastructureError, LaunchError, NodeHandle};

#[cfg(feature = "metrics")]
use crate::containers::WithMetrics;

/// Executor, directories, database, API config, and the optional metrics
/// attachment needed to launch a node.
///
/// One stage type carries every launch input: the binary attaches its metrics
/// recorder here so it installs before any subsystem records, then flows the
/// same context into `with_protocol`.
#[derive(Clone)]
pub struct LaunchContext<A = ()> {
    pub executor: TaskExecutor,
    pub dirs: DataDirs,
    pub api: A,
    pub database: DatabaseConfig,
    /// Metrics recorder and server config, threaded from `with_metrics` to
    /// `start_metrics_server`.
    #[cfg(feature = "metrics")]
    metrics: Option<WithMetrics>,
}

impl<A> LaunchContext<A> {
    /// Defaults to an in-memory database configuration.
    pub fn new(executor: TaskExecutor, dirs: DataDirs, api: A) -> Self {
        Self {
            executor,
            dirs,
            api,
            database: DatabaseConfig::default(),
            #[cfg(feature = "metrics")]
            metrics: None,
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
        api: A,
        executor: TaskExecutor,
        dirs: DataDirs,
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

#[cfg(feature = "metrics")]
impl<A> WithLaunchContext<A> {
    /// Install the process-global Prometheus recorder for the configured metrics
    /// server.
    ///
    /// Runs before `with_protocol` so the recorder is in place before any
    /// subsystem records. Each protocol crate exports its histogram bucket
    /// requirements as `HISTOGRAM_BUCKETS`; collect them all and pass here.
    pub fn with_metrics(
        mut self,
        config: Option<vertex_observability::MetricsServerConfig>,
        histogram_buckets: &[vertex_observability::HistogramBucketConfig],
    ) -> eyre::Result<Self> {
        let recorder = if let Some(ref cfg) = config {
            let recorder = vertex_observability::install_prometheus_recorder_with_buckets(
                cfg.prefix(),
                histogram_buckets,
            )?;
            recorder.spawn_upkeep(&self.ctx.executor, cfg.upkeep_interval_secs());
            Some(std::sync::Arc::new(recorder))
        } else {
            None
        };
        self.ctx.metrics = Some(WithMetrics::new(config, recorder));
        Ok(self)
    }

    /// Start the metrics HTTP server when both a server config and a recorder are
    /// present; otherwise a no-op that returns the context unchanged.
    pub async fn start_metrics_server(self) -> eyre::Result<Self> {
        let Some(metrics) = self.ctx.metrics.as_ref() else {
            return Ok(self);
        };
        if let (Some(config), Some(recorder)) = (metrics.config(), metrics.recorder()) {
            let hooks_builder = vertex_observability::Hooks::builder()
                .with_hook(vertex_observability::process_metrics_hook());
            #[cfg(feature = "jemalloc")]
            let hooks_builder =
                hooks_builder.with_hook(vertex_observability::jemalloc_metrics_hook());
            let hooks = hooks_builder.build();
            let server = vertex_observability::MetricsServer::from_config(
                config,
                recorder.handle().clone(),
                hooks,
            );
            server.start(&self.ctx.executor).await?;
            tracing::info!(addr = %config.addr(), "Metrics server started");
        }
        Ok(self)
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
    ///
    /// The components register through the protocol's serve view (`P::serve_view`),
    /// a transport-specific projection; the node handle keeps the bare components.
    pub async fn launch_with<Tr: Transport>(
        self,
    ) -> Result<NodeHandle<P::Components>, LaunchError<P::BuildError>>
    where
        P::ServeView: ServeWith<Tr>,
    {
        use tracing::info;

        info!("Data directory: {}", self.ctx.dirs.root.display());
        info!("RPC address: {}", self.ctx.grpc_addr());

        let addr = self.ctx.grpc_addr();

        let components = P::launch(self.config, &self.ctx)
            .await
            .map_err(LaunchError::Protocol)?;

        let mut registry = Tr::Registry::default();
        P::serve_view(&components).register(&mut registry);

        let server = Tr::into_server(registry, addr)
            .map_err(|e| InfrastructureError::Transport(e.into()))?;

        let shutdown_executor = self.ctx.executor.clone();
        self.ctx
            .executor
            .spawn_critical_with_graceful_shutdown_signal(
                "rpc.server",
                move |shutdown| async move {
                    if let Err(e) = server.serve_with_shutdown(shutdown.ignore_guard()).await {
                        tracing::error!(error = %e, "RPC server error");
                    }
                    // The server resolves on the shutdown signal in the normal
                    // path; an exit for any other reason (a post-bind serve
                    // failure) requests graceful shutdown so the node does not
                    // linger without its RPC endpoint.
                    let _ = shutdown_executor.initiate_graceful_shutdown();
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
        P::ServeView: ServeWith<GrpcTransport>,
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
