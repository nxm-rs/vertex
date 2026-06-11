//! Unified observability for Vertex: logging, tracing, and metrics.
//!
//! This crate provides the "heavy" observability infrastructure: tracing setup,
//! Prometheus recorder, metrics HTTP server, and profiling. Lightweight metric
//! primitives (guards, macros, labels) live in [`vertex_metrics`] and are
//! re-exported here for convenience.
//!
//! The server stack (tracing subscriber, OTLP exporters, the Prometheus
//! recorder, the metrics HTTP server, and profiling) lives behind the default
//! `server` feature. A wasm client depends on this crate with
//! `default-features = false` to keep only the platform-neutral metric
//! primitives: the recording macros, RAII guards, label utilities, and the
//! histogram bucket presets. The server stack pulls `axum` -> `tokio[net]` ->
//! `mio`, which does not build for `wasm32`.

#[cfg(feature = "server")]
mod config;
#[cfg(feature = "server")]
mod format;
#[cfg(feature = "server")]
mod guard;
#[cfg(feature = "server")]
mod layers;
pub mod metrics;
#[cfg(feature = "server")]
pub mod profiling;
#[cfg(feature = "server")]
mod tracer;

#[cfg(feature = "server")]
pub use config::{MetricsServerConfig, OtlpConfig, OtlpLogsConfig, StdoutConfig};
#[cfg(feature = "server")]
pub use format::LogFormat;
#[cfg(feature = "server")]
pub use guard::TracingGuard;
#[cfg(all(feature = "server", feature = "jemalloc"))]
pub use metrics::jemalloc_metrics_hook;
// Platform-neutral histogram presets, always available (including wasm).
pub use metrics::{
    CONNECTION_LIFETIME, DURATION_FINE, DURATION_NETWORK, DURATION_SECONDS, HistogramBucketConfig,
    LOCK_CONTENTION, POLL_DURATION,
};
// Native server stack: recorder, HTTP server, process hooks.
#[cfg(feature = "server")]
pub use metrics::{
    HistogramRegistry, Hook, Hooks, HooksBuilder, MetricsServer, PrometheusRecorder,
    install_prometheus_recorder, install_prometheus_recorder_with_buckets,
    install_prometheus_recorder_with_prefix, process_metrics_hook,
};
#[cfg(feature = "server")]
pub use tracer::VertexTracer;

// Re-export all metric primitives from vertex-metrics.
pub use vertex_metrics::{
    // RAII guards
    CounterGuard,
    GaugeGuard,
    LabelValue,
    OperationGuard,
    StreamGuard,
    TimingGuard,
    // Lazy metric macros
    lazy_counter,
    lazy_gauge,
    lazy_histogram,
    // Lock timing helpers
    timed_lock,
    timed_read,
    timed_write,
};

/// Re-export guards module for qualified access.
pub use vertex_metrics::guards;

/// Re-export labels module for qualified access.
pub use vertex_metrics::labels;

/// Re-export the metrics crate.
pub use ::metrics as metrics_crate;

/// Re-export strum for deriving LabelValue.
pub use vertex_metrics::strum;
