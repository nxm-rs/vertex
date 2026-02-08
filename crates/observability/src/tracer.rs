//! VertexTracer builder for unified observability initialization.

use crate::{layers, FileConfig, OtlpConfig, StdoutConfig, TracingGuard};

/// Builder for initializing the tracing/logging stack.
///
/// Configures stdout, file, and OTLP layers, then initializes them as a unified subscriber.
#[derive(Debug, Default)]
pub struct VertexTracer {
    stdout: Option<StdoutConfig>,
    file: Option<FileConfig>,
    otlp: Option<OtlpConfig>,
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

    /// Configure file logging.
    pub fn with_file(mut self, config: FileConfig) -> Self {
        self.file = Some(config);
        self
    }

    /// Configure OpenTelemetry OTLP tracing.
    pub fn with_otlp(mut self, config: OtlpConfig) -> Self {
        self.otlp = Some(config);
        self
    }

    /// Initialize the tracing subscriber with configured layers.
    ///
    /// Returns a guard that must be held for the program's lifetime.
    pub fn init(self) -> eyre::Result<TracingGuard> {
        layers::build_and_init(self.stdout.as_ref(), self.file.as_ref(), self.otlp.as_ref())
    }
}
