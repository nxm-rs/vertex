//! Observability guards for resource cleanup.

#[cfg(feature = "otlp")]
use opentelemetry_sdk::{logs::SdkLoggerProvider, trace::SdkTracerProvider};

/// Guard that manages observability lifecycle.
///
/// Must be held for the duration of the program. With the `otlp` feature, on
/// drop it shuts down the OpenTelemetry tracer and logger providers. Without
/// `otlp` it is an empty marker that keeps the subscriber API uniform.
pub struct TracingGuard {
    #[cfg(feature = "otlp")]
    tracer_provider: Option<SdkTracerProvider>,
    #[cfg(feature = "otlp")]
    logger_provider: Option<SdkLoggerProvider>,
}

impl TracingGuard {
    #[cfg(feature = "otlp")]
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
            #[cfg(feature = "otlp")]
            tracer_provider: None,
            #[cfg(feature = "otlp")]
            logger_provider: None,
        }
    }
}

#[cfg(feature = "otlp")]
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
