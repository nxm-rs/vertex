//! Observability guards for resource cleanup.

use opentelemetry_sdk::{logs::SdkLoggerProvider, trace::SdkTracerProvider};

/// Guard that manages observability lifecycle.
///
/// Must be held for the duration of the program. On drop:
/// - Shuts down the OpenTelemetry tracer and logger providers
pub struct TracingGuard {
    tracer_provider: Option<SdkTracerProvider>,
    logger_provider: Option<SdkLoggerProvider>,
}

impl TracingGuard {
    pub(crate) fn new(
        tracer_provider: Option<SdkTracerProvider>,
        logger_provider: Option<SdkLoggerProvider>,
    ) -> Self {
        Self {
            tracer_provider,
            logger_provider,
        }
    }

    /// Create a no-op guard (when tracing is disabled).
    pub fn noop() -> Self {
        Self {
            tracer_provider: None,
            logger_provider: None,
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
