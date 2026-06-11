//! Prometheus metrics infrastructure.
//!
//! The bucket presets and [`buckets::HistogramBucketConfig`] are platform
//! neutral and always available. The recorder, HTTP server, process hooks, and
//! upkeep task live behind the `server` feature because they pull `axum` ->
//! `tokio[net]` -> `mio`, which does not build for `wasm32`.

pub mod buckets;
#[cfg(feature = "server")]
mod hooks;
#[cfg(feature = "server")]
mod process;
#[cfg(feature = "server")]
mod recorder;
#[cfg(feature = "server")]
mod server;
#[cfg(feature = "server")]
mod task;

pub use buckets::{
    CONNECTION_LIFETIME, DURATION_FINE, DURATION_NETWORK, DURATION_SECONDS, HistogramBucketConfig,
    LOCK_CONTENTION, POLL_DURATION,
};
#[cfg(feature = "server")]
pub use hooks::{Hook, Hooks, HooksBuilder};
#[cfg(all(feature = "server", feature = "jemalloc"))]
pub use process::jemalloc_metrics_hook;
#[cfg(feature = "server")]
pub use process::process_metrics_hook;
#[cfg(feature = "server")]
pub use recorder::{
    HistogramRegistry, PrometheusRecorder, install_prometheus_recorder,
    install_prometheus_recorder_with_buckets, install_prometheus_recorder_with_prefix,
};
#[cfg(feature = "server")]
pub use server::MetricsServer;
