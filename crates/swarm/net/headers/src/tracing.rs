//! Distributed tracing context propagation.
//!
//! This module provides OpenTelemetry-compatible context propagation for
//! distributed tracing across the Swarm network. When a peer initiates a
//! request, their trace context is serialized into headers. The receiving
//! peer extracts this context and links their span to the trace.
//!
//! # Usage with Grafana Tempo
//!
//! To send traces to Grafana Tempo:
//!
//! 1. Set up the OpenTelemetry OTLP exporter pointing to Tempo
//! 2. Register a `TraceContextPropagator` as the global propagator
//! 3. Use `tracing-opentelemetry` layer in your subscriber
//!
//! This module will then automatically propagate trace context in headers.

use std::collections::HashMap;

use bytes::Bytes;
use opentelemetry::{
    global,
    propagation::{Extractor, Injector},
};
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// Header name for tracing span context propagation.
///
/// This follows the Bee protocol convention. The actual trace context format
/// (W3C TraceContext, Jaeger, B3, etc.) depends on the configured propagator.
pub const HEADER_NAME_TRACING_SPAN_CONTEXT: &str = "tracing-span-context";

/// Wrapper around headers map that implements OpenTelemetry's `Injector` trait.
///
/// Used to inject trace context into outgoing request headers.
pub struct HeaderInjector<'a>(pub &'a mut HashMap<String, Bytes>);

impl Injector for HeaderInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        self.0.insert(key.to_string(), Bytes::from(value));
    }
}

/// Wrapper around headers map that implements OpenTelemetry's `Extractor` trait.
///
/// Used to extract trace context from incoming request headers.
pub struct HeaderExtractor<'a>(pub &'a HashMap<String, Bytes>);

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0
            .get(key)
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
    }
}

/// Inject the current span's trace context into headers.
///
/// Uses the globally configured propagator (e.g., W3C TraceContext) to serialize
/// the current span's context into the headers map. If there's no active span
/// or no propagator configured, this is a no-op.
///
/// # Example
///
/// ```ignore
/// let mut headers = HashMap::new();
/// inject_trace_context(&mut headers);
/// // headers now contains trace context if a span is active
/// ```
pub fn inject_trace_context(headers: &mut HashMap<String, Bytes>) {
    let current_span = Span::current();
    if current_span.is_none() {
        return;
    }

    // Get the OpenTelemetry context from the current tracing span
    let context = current_span.context();

    // Inject using the globally configured propagator
    global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&context, &mut HeaderInjector(headers));
    });
}

/// Extract trace context from headers and set it as the parent of the current span.
///
/// Uses the globally configured propagator to deserialize trace context from
/// the headers. If valid context is found, it's set as the parent of the
/// current span, linking this span to the distributed trace.
///
/// # Example
///
/// ```ignore
/// // In an inbound request handler:
/// let span = tracing::info_span!("handle_request");
/// let _guard = span.enter();
/// extract_trace_context(&headers); // Links span to remote trace
/// ```
pub fn extract_trace_context(headers: &HashMap<String, Bytes>) {
    let parent_context =
        global::get_text_map_propagator(|propagator| propagator.extract(&HeaderExtractor(headers)));

    // Set the extracted context as the parent of the current span
    // Ignore the result - if the span isn't recording, that's fine
    let _ = Span::current().set_parent(parent_context);
}

/// Check if headers contain trace context.
pub fn has_trace_context(headers: &HashMap<String, Bytes>) -> bool {
    // Check for common trace context headers
    // W3C TraceContext uses "traceparent"
    // Jaeger uses "uber-trace-id"
    // B3 uses "x-b3-traceid"
    headers.contains_key("traceparent")
        || headers.contains_key("uber-trace-id")
        || headers.contains_key("x-b3-traceid")
        || headers.contains_key(HEADER_NAME_TRACING_SPAN_CONTEXT)
}

/// Create a span for a protocol operation, extracting any parent context from headers.
///
/// This is a convenience function that:
/// 1. Creates a new span for the protocol operation
/// 2. Extracts any trace context from headers
/// 3. Sets the extracted context as the span's parent
///
/// The returned span will be part of the distributed trace if context was found.
pub fn span_from_headers(
    protocol: &str,
    direction: &str,
    headers: &HashMap<String, Bytes>,
) -> Span {
    let span = tracing::info_span!("protocol", protocol = protocol, direction = direction,);

    // Extract and set parent context within the span
    let _guard = span.enter();
    extract_trace_context(headers);
    drop(_guard);

    span
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header_extractor() {
        let mut headers = HashMap::new();
        headers.insert("traceparent".to_string(), Bytes::from("00-trace-span-01"));
        headers.insert("custom-header".to_string(), Bytes::from("value"));

        let extractor = HeaderExtractor(&headers);
        assert_eq!(extractor.get("traceparent"), Some("00-trace-span-01"));
        assert_eq!(extractor.get("custom-header"), Some("value"));
        assert_eq!(extractor.get("missing"), None);

        let keys = extractor.keys();
        assert!(keys.contains(&"traceparent"));
        assert!(keys.contains(&"custom-header"));
    }

    #[test]
    fn test_header_injector() {
        let mut headers = HashMap::new();
        {
            let mut injector = HeaderInjector(&mut headers);
            injector.set("traceparent", "00-abc-def-01".to_string());
        }

        assert_eq!(
            headers.get("traceparent"),
            Some(&Bytes::from("00-abc-def-01"))
        );
    }

    #[test]
    fn test_has_trace_context() {
        let mut headers = HashMap::new();
        assert!(!has_trace_context(&headers));

        headers.insert("traceparent".to_string(), Bytes::from("..."));
        assert!(has_trace_context(&headers));

        headers.clear();
        headers.insert("uber-trace-id".to_string(), Bytes::from("..."));
        assert!(has_trace_context(&headers));
    }
}
