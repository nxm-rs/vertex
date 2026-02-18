//! Type-safe launch context with state accumulation.

use std::path::Path;
use std::sync::Arc;

use vertex_node_api::InfrastructureContext;
use vertex_node_core::dirs::DataDirs;
use vertex_observability::{
    Hooks, MetricsServer, MetricsServerConfig, PrometheusRecorder,
    install_prometheus_recorder_with_prefix, process_metrics_hook,
};
use vertex_tasks::TaskExecutor;

use crate::containers::WithMetrics;

/// Pairs two values, preserving access to both during launch sequence.
#[derive(Clone, Copy, Debug)]
pub struct Attached<L, R> {
    left: L,
    right: R,
}

impl<L, R> Attached<L, R> {
    pub const fn new(left: L, right: R) -> Self {
        Self { left, right }
    }

    pub const fn left(&self) -> &L {
        &self.left
    }

    pub const fn right(&self) -> &R {
        &self.right
    }

    pub fn into_left(self) -> L {
        self.left
    }

    pub fn into_right(self) -> R {
        self.right
    }

    pub fn map_left<F, T>(self, f: F) -> Attached<T, R>
    where
        F: FnOnce(L) -> T,
    {
        Attached::new(f(self.left), self.right)
    }

    pub fn map_right<F, T>(self, f: F) -> Attached<L, T>
    where
        F: FnOnce(R) -> T,
    {
        Attached::new(self.left, f(self.right))
    }
}

/// Launch context with an attachment.
#[derive(Debug, Clone)]
pub struct LaunchContextWith<T> {
    executor: TaskExecutor,
    dirs: DataDirs,
    attachment: T,
}

impl<T> LaunchContextWith<T> {
    pub(crate) fn new(executor: TaskExecutor, dirs: DataDirs, attachment: T) -> Self {
        Self {
            executor,
            dirs,
            attachment,
        }
    }

    pub fn executor(&self) -> &TaskExecutor {
        &self.executor
    }

    pub fn dirs(&self) -> &DataDirs {
        &self.dirs
    }

    pub fn attachment(&self) -> &T {
        &self.attachment
    }

    pub fn into_attachment(self) -> T {
        self.attachment
    }

    /// Attach another value, preserving access to previous state.
    pub fn attach<A>(self, attachment: A) -> LaunchContextWith<Attached<T, A>> {
        LaunchContextWith::new(
            self.executor,
            self.dirs,
            Attached::new(self.attachment, attachment),
        )
    }

    pub fn inspect<F>(self, f: F) -> Self
    where
        F: FnOnce(&Self),
    {
        f(&self);
        self
    }
}

impl LaunchContextWith<WithMetrics> {
    pub fn prometheus_recorder(&self) -> Option<&Arc<PrometheusRecorder>> {
        self.attachment.recorder()
    }

    /// Start the metrics HTTP server if configured.
    pub async fn start_metrics_server(self) -> eyre::Result<Self> {
        let has_config = self.attachment.config().is_some();
        let has_recorder = self.attachment.recorder().is_some();
        tracing::debug!(has_config, has_recorder, "Checking metrics server prerequisites");

        if let (Some(config), Some(recorder)) =
            (self.attachment.config(), self.attachment.recorder())
        {
            tracing::debug!(addr = %config.addr(), "Starting metrics server");
            let hooks_builder = Hooks::builder()
                .with_hook(process_metrics_hook());
            #[cfg(feature = "jemalloc")]
            let hooks_builder = hooks_builder.with_hook(vertex_observability::jemalloc_metrics_hook());
            let hooks = hooks_builder.build();
            let server = MetricsServer::from_config(config, recorder.handle().clone(), hooks);
            server.start(&self.executor).await?;
            tracing::info!(addr = %config.addr(), "Metrics server started");
        } else {
            tracing::debug!("Metrics server not started (config or recorder missing)");
        }
        Ok(self)
    }
}

impl<T: Send + Sync> InfrastructureContext for LaunchContextWith<T> {
    fn executor(&self) -> &TaskExecutor {
        &self.executor
    }

    fn data_dir(&self) -> &Path {
        &self.dirs.network
    }
}

/// Extension trait for creating launch contexts with metrics.
pub trait LaunchContextExt {
    /// Attach metrics infrastructure.
    fn with_metrics(
        self,
        config: Option<MetricsServerConfig>,
    ) -> eyre::Result<LaunchContextWith<WithMetrics>>;
}

impl LaunchContextExt for (TaskExecutor, DataDirs) {
    fn with_metrics(
        self,
        config: Option<MetricsServerConfig>,
    ) -> eyre::Result<LaunchContextWith<WithMetrics>> {
        let (executor, dirs) = self;

        let recorder = if let Some(ref cfg) = config {
            tracing::debug!(addr = %cfg.addr(), prefix = %cfg.prefix(), "Installing prometheus recorder");
            let recorder = install_prometheus_recorder_with_prefix(cfg.prefix())?;
            recorder.spawn_upkeep(&executor, cfg.upkeep_interval_secs());
            tracing::debug!("Prometheus recorder installed successfully");
            Some(Arc::new(recorder))
        } else {
            tracing::debug!("Metrics disabled, skipping prometheus recorder");
            None
        };

        let attachment = WithMetrics::new(config, recorder);
        Ok(LaunchContextWith::new(executor, dirs, attachment))
    }
}
