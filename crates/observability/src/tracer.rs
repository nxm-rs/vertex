//! VertexTracer builder for unified observability initialization.

#[cfg(feature = "otlp")]
use crate::{OtlpConfig, OtlpLogsConfig};
use crate::{StdoutConfig, TracingGuard, layers};

/// Builder for initializing the tracing/logging stack.
///
/// Configures the stdout layer and, with the `otlp` feature, the OTLP trace and
/// log layers, then initializes them as a unified subscriber.
#[derive(Debug, Default)]
pub struct VertexTracer {
    stdout: Option<StdoutConfig>,
    #[cfg(feature = "otlp")]
    otlp: Option<OtlpConfig>,
    #[cfg(feature = "otlp")]
    otlp_logs: Option<OtlpLogsConfig>,
}

impl VertexTracer {
    /// Create a new tracer builder with no layers configured.
    pub fn new() -> Self {
        Self::default()
    }

    /// Configure stdout/console logging.
    pub fn with_stdout(mut self, config: StdoutConfig) -> Self {
        self.stdout = Some(config);
        self
    }

    /// Configure OpenTelemetry OTLP tracing.
    #[cfg(feature = "otlp")]
    pub fn with_otlp(mut self, config: OtlpConfig) -> Self {
        self.otlp = Some(config);
        self
    }

    /// Configure OTLP log export (e.g., to Loki).
    #[cfg(feature = "otlp")]
    pub fn with_otlp_logs(mut self, config: OtlpLogsConfig) -> Self {
        self.otlp_logs = Some(config);
        self
    }

    /// Initialize the tracing subscriber with configured layers.
    ///
    /// Returns a guard that must be held for the program's lifetime.
    pub fn init(self) -> eyre::Result<TracingGuard> {
        layers::build_and_init(
            self.stdout.as_ref(),
            #[cfg(feature = "otlp")]
            self.otlp.as_ref(),
            #[cfg(feature = "otlp")]
            self.otlp_logs.as_ref(),
        )
    }
}
