//! Observability guards for resource cleanup.

use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_appender::non_blocking::WorkerGuard;

/// Guard that manages observability lifecycle.
///
/// Must be held for the duration of the program. On drop:
/// - Flushes buffered file logs
/// - Shuts down the OpenTelemetry tracer provider
pub struct TracingGuard {
    provider: Option<SdkTracerProvider>,
    _file_guard: Option<WorkerGuard>,
}

impl TracingGuard {
    pub(crate) fn new(provider: Option<SdkTracerProvider>, file_guard: Option<WorkerGuard>) -> Self {
        Self {
            provider,
            _file_guard: file_guard,
        }
    }

    /// Create a no-op guard (when tracing is disabled).
    pub fn noop() -> Self {
        Self {
            provider: None,
            _file_guard: None,
        }
    }
}

impl Drop for TracingGuard {
    fn drop(&mut self) {
        if let Some(provider) = self.provider.take()
            && let Err(e) = provider.shutdown()
        {
            eprintln!("Error shutting down tracer provider: {e:?}");
        }
    }
}
