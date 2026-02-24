//! Observability guards for resource cleanup.

use opentelemetry_sdk::{logs::SdkLoggerProvider, trace::SdkTracerProvider};
use tracing_appender::non_blocking::WorkerGuard;

/// Guard that manages observability lifecycle.
///
/// Must be held for the duration of the program. On drop:
/// - Flushes buffered file logs
/// - Shuts down the OpenTelemetry tracer and logger providers
pub struct TracingGuard {
    tracer_provider: Option<SdkTracerProvider>,
    logger_provider: Option<SdkLoggerProvider>,
    _file_guard: Option<WorkerGuard>,
}

impl TracingGuard {
    pub(crate) fn new(
        tracer_provider: Option<SdkTracerProvider>,
        logger_provider: Option<SdkLoggerProvider>,
        file_guard: Option<WorkerGuard>,
    ) -> Self {
        Self {
            tracer_provider,
            logger_provider,
            _file_guard: file_guard,
        }
    }

    /// Create a no-op guard (when tracing is disabled).
    pub fn noop() -> Self {
        Self {
            tracer_provider: None,
            logger_provider: None,
            _file_guard: None,
        }
    }
}

impl Drop for TracingGuard {
    fn drop(&mut self) {
        if let Some(provider) = self.logger_provider.take()
            && let Err(e) = provider.shutdown()
        {
            eprintln!("Error shutting down logger provider: {e:?}");
        }
        if let Some(provider) = self.tracer_provider.take()
            && let Err(e) = provider.shutdown()
        {
            eprintln!("Error shutting down tracer provider: {e:?}");
        }
    }
}
