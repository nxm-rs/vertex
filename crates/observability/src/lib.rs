//! Unified observability for Vertex: logging, tracing, and metrics.

mod config;
mod format;
mod guard;
pub mod guards;
mod label_value;
pub mod labels;
mod layers;
pub mod metrics;
mod tracer;

pub use config::{FileConfig, MetricsServerConfig, OtlpConfig, StdoutConfig};
pub use format::LogFormat;
pub use guard::TracingGuard;
pub use guards::{CounterGuard, GaugeGuard, OperationGuard, TimingGuard};
pub use label_value::LabelValue;
pub use metrics::{
    Hook, Hooks, HooksBuilder, MetricsServer, PrometheusRecorder, install_prometheus_recorder,
    install_prometheus_recorder_with_prefix,
};
pub use tracer::VertexTracer;

// Re-export lazy metric macros from vertex-tasks.
pub use vertex_tasks::{lazy_counter, lazy_gauge, lazy_histogram};

/// Re-export the metrics crate.
pub use ::metrics as metrics_crate;

/// Re-export strum for deriving LabelValue.
pub use ::strum;
