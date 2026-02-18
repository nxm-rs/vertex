//! Unified observability for Vertex: logging, tracing, and metrics.
//!
//! This crate provides the "heavy" observability infrastructure: tracing setup,
//! Prometheus recorder, metrics HTTP server, and profiling. Lightweight metric
//! primitives (guards, macros, labels) live in [`vertex_metrics`] and are
//! re-exported here for convenience.

mod config;
mod format;
mod guard;
mod layers;
pub mod metrics;
pub mod profiling;
mod tracer;

pub use config::{FileConfig, MetricsServerConfig, OtlpConfig, StdoutConfig};
pub use format::LogFormat;
pub use guard::TracingGuard;
pub use metrics::{
    CONNECTION_LIFETIME, DURATION_FINE, DURATION_NETWORK, DURATION_SECONDS, HistogramBucketConfig,
    HistogramRegistry, Hook, Hooks, HooksBuilder, LOCK_CONTENTION, MetricsServer, POLL_DURATION,
    PrometheusRecorder, install_prometheus_recorder, install_prometheus_recorder_with_buckets,
    install_prometheus_recorder_with_prefix, process_metrics_hook,
};
#[cfg(feature = "jemalloc")]
pub use metrics::jemalloc_metrics_hook;
pub use tracer::VertexTracer;

// Re-export all metric primitives from vertex-metrics.
pub use vertex_metrics::{
    // RAII guards
    CounterGuard, GaugeGuard, LabelValue, OperationGuard, TimingGuard,
    // Lazy metric macros
    lazy_counter, lazy_gauge, lazy_histogram,
    // Lock timing helpers
    timed_lock, timed_read, timed_write,
};

/// Re-export guards module for qualified access.
pub use vertex_metrics::guards;

/// Re-export labels module for qualified access.
pub use vertex_metrics::labels;

/// Re-export the metrics crate.
pub use ::metrics as metrics_crate;

/// Re-export strum for deriving LabelValue.
pub use vertex_metrics::strum;
