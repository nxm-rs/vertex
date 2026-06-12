//! Prometheus metrics infrastructure.
//!
//! The bucket presets and [`buckets::HistogramBucketConfig`] are platform
//! neutral and always available; they live in the [`vertex_metrics`] leaf and
//! are re-exported here as `metrics::buckets` for source compatibility. The
//! recorder, process hooks, and histogram registry live behind the `prometheus`
//! feature; the HTTP server and its upkeep task live behind `http-server`
//! because they pull `axum` -> `tokio[net]` -> `mio`, which does not build for
//! `wasm32`.

/// Histogram bucket presets, re-exported from the [`vertex_metrics`] leaf.
pub use vertex_metrics::buckets;
#[cfg(feature = "prometheus")]
mod hooks;
#[cfg(feature = "prometheus")]
mod process;
#[cfg(feature = "prometheus")]
mod recorder;
#[cfg(feature = "http-server")]
mod server;
#[cfg(feature = "prometheus")]
mod task;

pub use buckets::{
    CONNECTION_LIFETIME, DURATION_FINE, DURATION_NETWORK, DURATION_SECONDS, HistogramBucketConfig,
    LOCK_CONTENTION, POLL_DURATION,
};
#[cfg(feature = "prometheus")]
pub use hooks::{Hook, Hooks, HooksBuilder};
#[cfg(all(feature = "prometheus", feature = "jemalloc"))]
pub use process::jemalloc_metrics_hook;
#[cfg(feature = "prometheus")]
pub use process::process_metrics_hook;
#[cfg(feature = "prometheus")]
pub use recorder::{
    HistogramRegistry, PrometheusRecorder, install_prometheus_recorder,
    install_prometheus_recorder_with_buckets, install_prometheus_recorder_with_prefix,
};
#[cfg(feature = "http-server")]
pub use server::MetricsServer;
