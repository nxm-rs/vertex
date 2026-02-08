//! Tracing CLI arguments.

use clap::Args;
use serde::{Deserialize, Serialize};
use vertex_observability::OtlpConfig;

/// Default OTLP endpoint.
const DEFAULT_OTLP_ENDPOINT: &str = "http://localhost:4317";

/// Default service name for traces.
const DEFAULT_SERVICE_NAME: &str = "vertex-swarm";

/// Default sampling ratio (1.0 = all traces).
const DEFAULT_SAMPLING_RATIO: f64 = 1.0;

/// OpenTelemetry tracing configuration.
#[derive(Debug, Args, Clone, Serialize, Deserialize)]
#[command(next_help_heading = "Tracing")]
#[serde(default)]
pub struct TracingArgs {
    /// Enable OpenTelemetry tracing to OTLP endpoint.
    #[arg(long = "tracing", id = "tracing.enabled")]
    pub enabled: bool,

    /// OTLP gRPC endpoint (e.g., "http://localhost:4317" for Tempo/Jaeger).
    #[arg(long = "tracing.endpoint", id = "tracing.endpoint", default_value = DEFAULT_OTLP_ENDPOINT)]
    pub endpoint: String,

    /// Service name reported in traces.
    #[arg(long = "tracing.service-name", id = "tracing.service-name", default_value = DEFAULT_SERVICE_NAME)]
    pub service_name: String,

    /// Sampling ratio (0.0 to 1.0). Use 1.0 for all traces, lower for high-volume.
    #[arg(long = "tracing.sampling-ratio", id = "tracing.sampling-ratio", default_value_t = DEFAULT_SAMPLING_RATIO)]
    pub sampling_ratio: f64,
}

impl Default for TracingArgs {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: DEFAULT_OTLP_ENDPOINT.to_string(),
            service_name: DEFAULT_SERVICE_NAME.to_string(),
            sampling_ratio: DEFAULT_SAMPLING_RATIO,
        }
    }
}

impl TracingArgs {
    /// Build OTLP tracing config.
    ///
    /// Returns None if tracing is disabled.
    pub fn otlp_config(&self) -> Option<OtlpConfig> {
        if !self.enabled {
            return None;
        }

        Some(OtlpConfig::new(
            self.endpoint.clone(),
            self.service_name.clone(),
            self.sampling_ratio,
        ))
    }
}
