//! Unified observability for Vertex: logging, tracing, and metrics.
//!
//! This crate provides the "heavy" observability infrastructure: tracing setup,
//! Prometheus recorder, metrics HTTP server, and profiling. Lightweight metric
//! primitives (guards, macros, labels) live in [`vertex_metrics`] and are
//! re-exported here for convenience.
//!
//! The native stack is split into orthogonal features so a consumer pulls only
//! what it uses:
//!
//! - `subscriber`: the tracing subscriber, `LogFormat` to layer conversion,
//!   [`VertexTracer`], and [`TracingGuard`].
//! - `otlp`: OTLP trace and log export layers on top of `subscriber`.
//! - `prometheus`: the Prometheus recorder, histogram registry, and process
//!   hooks.
//! - `http-server`: the `axum` metrics HTTP server on top of `prometheus`.
//! - `host`: an umbrella that unions the four slices (the full native stack).
//!
//! The plain config structs ([`StdoutConfig`], [`OtlpConfig`],
//! [`OtlpLogsConfig`], [`MetricsServerConfig`], [`LogFormat`]) and the
//! platform-neutral metric primitives (recording macros, RAII guards, label
//! utilities, histogram bucket presets) compile with no features enabled, so a
//! wasm client depends on this crate with `default-features = false` and keeps
//! only that light surface. The `http-server` slice pulls `axum` ->
//! `tokio[net]` -> `mio`, which does not build for `wasm32`.

mod config;
mod format;
#[cfg(feature = "subscriber")]
mod guard;
#[cfg(feature = "subscriber")]
mod layers;
pub mod metrics;
#[cfg(feature = "http-server")]
pub mod profiling;
#[cfg(feature = "subscriber")]
mod tracer;

// Plain config structs and the log format enum: always available, including on
// wasm, so a config-only consumer can name these types without the heavy stack.
pub use config::{MetricsServerConfig, OtlpConfig, OtlpLogsConfig, StdoutConfig};
pub use format::LogFormat;
#[cfg(feature = "subscriber")]
pub use guard::TracingGuard;
#[cfg(all(feature = "prometheus", feature = "jemalloc"))]
pub use metrics::jemalloc_metrics_hook;
// Platform-neutral histogram presets, always available (including wasm).
pub use metrics::{
    CONNECTION_LIFETIME, DURATION_FINE, DURATION_NETWORK, DURATION_SECONDS, HistogramBucketConfig,
    LOCK_CONTENTION, POLL_DURATION,
};
// Prometheus recorder, registry, and process hooks.
#[cfg(feature = "prometheus")]
pub use metrics::{
    HistogramRegistry, Hook, Hooks, HooksBuilder, PrometheusRecorder, install_prometheus_recorder,
    install_prometheus_recorder_with_buckets, install_prometheus_recorder_with_prefix,
    process_metrics_hook,
};
// Axum metrics HTTP server.
#[cfg(feature = "http-server")]
pub use metrics::MetricsServer;
#[cfg(feature = "subscriber")]
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
