//! Combined observability CLI arguments.

use clap::Args;
use serde::{Deserialize, Serialize};

use super::MetricsArgs;

/// Observability configuration for metrics.
///
/// Note: Logging is handled separately at the top level via `LogArgs`,
/// and tracing is handled at the top level via `TracingArgs`.
#[derive(Debug, Args, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ObservabilityArgs {
    /// Prometheus metrics configuration.
    #[command(flatten)]
    pub metrics: MetricsArgs,
}
