//! Combined observability CLI arguments.

use clap::Args;
use serde::{Deserialize, Serialize};

use super::{MetricsArgs, TracingArgs};

/// Observability configuration for metrics and tracing.
///
/// Note: Logging is handled separately at the top level via `LogArgs`
/// to avoid argument duplication. This struct only contains metrics and
/// tracing configuration.
#[derive(Debug, Args, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ObservabilityArgs {
    /// Prometheus metrics configuration.
    #[command(flatten)]
    pub metrics: MetricsArgs,

    /// OpenTelemetry tracing configuration.
    #[command(flatten)]
    pub tracing: TracingArgs,
}
