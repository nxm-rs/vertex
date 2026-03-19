//! Prometheus metrics recorder.

use std::collections::HashSet;

use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use metrics_util::layers::{PrefixLayer, Stack};
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use vertex_tasks::TaskExecutor;

use crate::MetricsServerConfig;

static PROMETHEUS_RECORDER: OnceLock<PrometheusRecorder> = OnceLock::new();

/// Custom histogram bucket configuration for a metric family.
///
/// Each crate that records histograms should export its bucket requirements
/// as a `pub const HISTOGRAM_BUCKETS: &[HistogramBucketConfig]` next to where
/// the histograms are recorded. The recorder collects these at install time.
#[derive(Debug, Clone, Copy)]
pub struct HistogramBucketConfig {
    /// Metric name suffix to match (e.g. `"handshake_duration_seconds"`).
    pub suffix: &'static str,
    /// Custom bucket boundaries (must be sorted ascending).
    pub buckets: &'static [f64],
}

/// Install the prometheus recorder with default "vertex" prefix and no custom buckets.
pub fn install_prometheus_recorder() -> eyre::Result<PrometheusRecorder> {
    install_prometheus_recorder_with_prefix("vertex")
}

/// Install the prometheus recorder with specific prefix and no custom buckets.
pub fn install_prometheus_recorder_with_prefix(prefix: &str) -> eyre::Result<PrometheusRecorder> {
    install_prometheus_recorder_with_buckets(prefix, &[])
}

/// Install the prometheus recorder with specific prefix and custom histogram buckets.
pub fn install_prometheus_recorder_with_buckets(
    prefix: &str,
    histogram_buckets: &[HistogramBucketConfig],
) -> eyre::Result<PrometheusRecorder> {
    match PROMETHEUS_RECORDER.get() {
        Some(recorder) => Ok(recorder.clone()),
        None => {
            let recorder = PrometheusRecorder::install(prefix, histogram_buckets)?;
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
            .field(
                "upkeep_started",
                &self.upkeep_started.load(Ordering::Relaxed),
            )
            .finish()
    }
}

impl PrometheusRecorder {
    fn install(prefix: &str, histogram_buckets: &[HistogramBucketConfig]) -> eyre::Result<Self> {
        // Configure histogram buckets for specific metrics.
        // Note: Buckets are set BEFORE the prefix layer, so use unprefixed names.
        let mut builder = PrometheusBuilder::new();
        for config in histogram_buckets {
            builder = builder.set_buckets_for_metric(
                Matcher::Suffix(config.suffix.to_string()),
                config.buckets,
            )?;
        }
        let recorder = builder.build_recorder();

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
        install_prometheus_recorder_with_buckets(config.prefix(), &[])
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
        executor.spawn_with_graceful_shutdown_signal(
            "metrics.upkeep",
            move |shutdown| async move {
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
            },
        );
    }
}

/// Collects histogram bucket configs from multiple crates, validating no duplicate suffixes.
pub struct HistogramRegistry {
    configs: Vec<HistogramBucketConfig>,
    seen: HashSet<&'static str>,
}

impl HistogramRegistry {
    pub fn new() -> Self {
        Self {
            configs: Vec::new(),
            seen: HashSet::new(),
        }
    }

    /// Register all configs from a slice. Panics on duplicate suffix.
    #[must_use]
    pub fn register_all(mut self, configs: &[HistogramBucketConfig]) -> Self {
        for config in configs {
            assert!(
                self.seen.insert(config.suffix),
                "duplicate histogram suffix: {:?}",
                config.suffix,
            );
            self.configs.push(*config);
        }
        self
    }

    /// Consume the registry and return the collected configs.
    pub fn build(self) -> Vec<HistogramBucketConfig> {
        self.configs
    }
}

impl Default for HistogramRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_collects_configs() {
        let a = &[HistogramBucketConfig {
            suffix: "alpha",
            buckets: &[1.0, 2.0],
        }];
        let b = &[HistogramBucketConfig {
            suffix: "beta",
            buckets: &[3.0, 4.0],
        }];

        let result = HistogramRegistry::new()
            .register_all(a)
            .register_all(b)
            .build();

        assert_eq!(result.len(), 2);
        assert_eq!(result[0].suffix, "alpha");
        assert_eq!(result[1].suffix, "beta");
    }

    #[test]
    #[should_panic(expected = "duplicate histogram suffix")]
    fn registry_panics_on_duplicate() {
        let a = &[HistogramBucketConfig {
            suffix: "dup",
            buckets: &[1.0],
        }];
        let b = &[HistogramBucketConfig {
            suffix: "dup",
            buckets: &[2.0],
        }];

        let _ = HistogramRegistry::new().register_all(a).register_all(b);
    }
}
