//! Prometheus metrics infrastructure.

pub mod buckets;
mod hooks;
mod process;
mod recorder;
mod server;

pub use buckets::{
    CONNECTION_LIFETIME, DURATION_FINE, DURATION_NETWORK, DURATION_SECONDS, LOCK_CONTENTION,
    POLL_DURATION,
};
pub use hooks::{Hook, Hooks, HooksBuilder};
pub use process::process_metrics_hook;
#[cfg(feature = "jemalloc")]
pub use process::jemalloc_metrics_hook;
pub use recorder::{
    HistogramBucketConfig, HistogramRegistry, PrometheusRecorder, install_prometheus_recorder,
    install_prometheus_recorder_with_buckets, install_prometheus_recorder_with_prefix,
};
pub use server::MetricsServer;
