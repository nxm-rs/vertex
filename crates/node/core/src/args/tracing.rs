//! Tracing CLI arguments.

use clap::Args;
use serde::{Deserialize, Serialize};
use vertex_observability::{OtlpConfig, OtlpLogsConfig};

/// Default OTLP endpoint.
const DEFAULT_OTLP_ENDPOINT: &str = "http://localhost:4317";

/// Default service name for traces.
const DEFAULT_SERVICE_NAME: &str = "vertex-swarm";

/// Default sampling ratio (1.0 = all traces).
const DEFAULT_SAMPLING_RATIO: f64 = 1.0;

/// Default OTLP logs endpoint (Loki HTTP, protobuf encoding).
const DEFAULT_OTLP_LOGS_ENDPOINT: &str = "http://localhost:3100/otlp/v1/logs";

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

    /// Enable OTLP log export (e.g., to Loki).
    #[arg(long = "tracing.logs", id = "tracing.logs")]
    pub logs_enabled: bool,

    /// OTLP log export endpoint (e.g., "http://localhost:3100" for Loki).
    #[arg(long = "tracing.logs-endpoint", id = "tracing.logs-endpoint", default_value = DEFAULT_OTLP_LOGS_ENDPOINT)]
    pub logs_endpoint: String,
}

impl Default for TracingArgs {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: DEFAULT_OTLP_ENDPOINT.to_string(),
            service_name: DEFAULT_SERVICE_NAME.to_string(),
            sampling_ratio: DEFAULT_SAMPLING_RATIO,
            logs_enabled: false,
            logs_endpoint: DEFAULT_OTLP_LOGS_ENDPOINT.to_string(),
        }
    }
}

impl TracingArgs {
    /// Build tracing config from CLI arguments.
    ///
    /// Returns `None` if tracing is disabled.
    pub fn tracing_config(&self) -> Option<OtlpConfig> {
        if !self.enabled {
            return None;
        }

        Some(OtlpConfig::new(
            self.endpoint.clone(),
            self.service_name.clone(),
            self.sampling_ratio,
        ))
    }

    /// Build tracing logs config from CLI arguments.
    ///
    /// Returns `None` if OTLP log export is disabled.
    pub fn tracing_logs_config(&self) -> Option<OtlpLogsConfig> {
        if !self.logs_enabled {
            return None;
        }

        Some(OtlpLogsConfig::new(
            self.logs_endpoint.clone(),
            self.service_name.clone(),
        ))
    }
}
