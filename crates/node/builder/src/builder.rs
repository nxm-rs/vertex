//! Type-state node builder for Vertex.

use std::path::Path;

use vertex_node_api::{InfrastructureContext, NodeBuildsProtocol, NodeProtocol};
use vertex_node_core::args::DatabaseConfig;
use vertex_node_core::dirs::DataDirs;
use vertex_tasks::TaskExecutor;

use crate::{LaunchError, NodeHandle};

#[cfg(feature = "metrics")]
use crate::containers::WithMetrics;

/// Executor, directories, database, and the optional metrics attachment needed
/// to launch a node.
///
/// One stage type carries every launch input: the binary attaches its metrics
/// recorder here so it installs before any subsystem records, then flows the
/// same context into `with_protocol`.
#[derive(Clone)]
pub struct LaunchContext {
    pub executor: TaskExecutor,
    pub dirs: DataDirs,
    pub database: DatabaseConfig,
    /// Metrics recorder and server config, threaded from `with_metrics` to
    /// `start_metrics_server`.
    #[cfg(feature = "metrics")]
    metrics: Option<WithMetrics>,
}

impl LaunchContext {
    /// Defaults to an in-memory database configuration.
    pub fn new(executor: TaskExecutor, dirs: DataDirs) -> Self {
        Self {
            executor,
            dirs,
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

impl InfrastructureContext for LaunchContext {
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

/// Node builder - first stage for adding launch context.
pub struct NodeBuilder;

impl NodeBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

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
    pub fn context(&self) -> &LaunchContext {
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
    pub fn with_protocol<C: NodeBuildsProtocol>(self, config: C) -> WithProtocol<C::Protocol> {
        tracing::info!("Protocol: {}", config.protocol_name());
        WithProtocol {
            ctx: self.ctx,
            config,
        }
    }
}

#[cfg(feature = "metrics")]
impl WithLaunchContext {
    /// Install the process-global Prometheus recorder for the configured metrics
    /// server.
    ///
    /// Runs before `with_protocol` so the recorder is in place before any
    /// subsystem records. Pass the launch-path bucket aggregate assembled by the
    /// protocol builder, not the individual per-crate consts.
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
pub struct WithProtocol<P: NodeProtocol> {
    ctx: LaunchContext,
    config: P::Config,
}

impl<P: NodeProtocol> WithProtocol<P>
where
    P::Config: NodeBuildsProtocol,
{
    pub fn context(&self) -> &LaunchContext {
        &self.ctx
    }

    /// Build and launch the protocol, returning a handle over the bare components.
    ///
    /// Serving is a separate step: attach a transport via
    /// [`NodeHandle::serve_with`](crate::NodeHandle::serve_with) after launch.
    pub async fn launch(self) -> Result<NodeHandle<P>, LaunchError<P::BuildError>> {
        use tracing::info;

        info!("Data directory: {}", self.ctx.dirs.root.display());

        let components = P::launch(self.config, &self.ctx)
            .await
            .map_err(LaunchError::Protocol)?;

        Ok(NodeHandle::new(
            components,
            self.ctx.executor.clone(),
            self.ctx.executor.on_shutdown_signal().clone(),
        ))
    }
}
