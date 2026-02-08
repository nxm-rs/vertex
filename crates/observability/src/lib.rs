//! Unified observability for Vertex Swarm: logging, tracing, and metrics.

mod config;
mod format;
mod guard;
mod layers;
pub mod metrics;
mod tracer;

pub use config::{FileConfig, MetricsServerConfig, OtlpConfig, StdoutConfig};
pub use format::LogFormat;
pub use guard::TracingGuard;
pub use metrics::{
    install_prometheus_recorder, install_prometheus_recorder_with_prefix, Hook, Hooks,
    HooksBuilder, MetricsServer, PrometheusRecorder,
};
pub use tracer::VertexTracer;

/// Re-export the metrics crate for convenience.
pub use ::metrics as metrics_crate;
