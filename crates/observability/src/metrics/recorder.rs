//! Prometheus metrics recorder.

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use metrics_util::layers::{PrefixLayer, Stack};
use once_cell::sync::OnceCell;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use vertex_tasks::TaskExecutor;

use crate::MetricsServerConfig;

static PROMETHEUS_RECORDER: OnceCell<PrometheusRecorder> = OnceCell::new();

/// Install the prometheus recorder with default "vertex" prefix.
pub fn install_prometheus_recorder() -> eyre::Result<PrometheusRecorder> {
    install_prometheus_recorder_with_prefix("vertex")
}

/// Install the prometheus recorder with specific prefix.
pub fn install_prometheus_recorder_with_prefix(prefix: &str) -> eyre::Result<PrometheusRecorder> {
    match PROMETHEUS_RECORDER.get() {
        Some(recorder) => Ok(recorder.clone()),
        None => {
            let recorder = PrometheusRecorder::install_with_prefix(prefix)?;
            Ok(PROMETHEUS_RECORDER.get_or_init(|| recorder).clone())
        }
    }
}

/// Handle to the prometheus metrics recorder.
#[derive(Clone)]
pub struct PrometheusRecorder {
    handle: PrometheusHandle,
    upkeep_started: Arc<AtomicBool>,
}

impl std::fmt::Debug for PrometheusRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrometheusRecorder")
            .field("upkeep_started", &self.upkeep_started.load(Ordering::Relaxed))
            .finish()
    }
}

impl PrometheusRecorder {
    fn install_with_prefix(prefix: &str) -> eyre::Result<Self> {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();

        Stack::new(recorder)
            .push(PrefixLayer::new(prefix))
            .install()?;

        Ok(Self {
            handle,
            upkeep_started: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Install recorder using configuration.
    pub fn install_with_config(config: &MetricsServerConfig) -> eyre::Result<Self> {
        install_prometheus_recorder_with_prefix(config.prefix())
    }

    pub fn handle(&self) -> &PrometheusHandle {
        &self.handle
    }

    /// Start the upkeep task for the prometheus recorder.
    pub fn spawn_upkeep(&self, executor: &TaskExecutor, interval_secs: u64) {
        if self
            .upkeep_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }

        let handle = self.handle.clone();
        executor.spawn_with_graceful_shutdown_signal("metrics_upkeep", move |shutdown| async move {
            let mut shutdown = std::pin::pin!(shutdown);
            let interval = std::time::Duration::from_secs(interval_secs);

            loop {
                tokio::select! {
                    guard = &mut shutdown => {
                        tracing::debug!("Metrics upkeep task shutting down");
                        drop(guard);
                        break;
                    }
                    _ = tokio::time::sleep(interval) => {
                        handle.run_upkeep();
                    }
                }
            }
        });
    }
}
