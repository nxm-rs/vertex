//! Prometheus metrics infrastructure.

mod hooks;
mod process;
mod recorder;
mod server;

pub use hooks::{Hook, Hooks, HooksBuilder};
pub use process::process_metrics_hook;
pub use recorder::{install_prometheus_recorder, install_prometheus_recorder_with_prefix, PrometheusRecorder};
pub use server::MetricsServer;
