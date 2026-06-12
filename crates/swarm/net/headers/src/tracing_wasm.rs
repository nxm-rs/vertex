//! No-op trace-context propagation for wasm32.
//!
//! The browser client has no OTLP backend to propagate distributed traces to, so
//! the inject and extract paths are no-ops here. This sibling mirrors the native
//! `tracing.rs` item names and signatures exactly so `upgrade.rs` and the crate
//! re-exports compile unchanged on `wasm32-unknown-unknown`. The on-wire header
//! field (`tracing-span-context`) is unaffected: native peers that send it are
//! still tolerated, and this client simply never reads or writes span context.

use std::collections::HashMap;

use bytes::Bytes;
use libp2p::PeerId;
use tracing::Span;
use vertex_swarm_primitives::OverlayAddress;

/// Peer identity context for enriching protocol spans.
///
/// Carried by headered streams so all protocol spans automatically
/// include the remote peer's identity (PeerId + overlay address).
#[derive(Debug, Clone)]
pub struct PeerContext {
    pub remote_peer_id: PeerId,
    pub remote_overlay: OverlayAddress,
}

/// Header name for tracing span context propagation.
///
/// The actual trace context format (W3C TraceContext, Jaeger, B3, etc.) depends
/// on the configured propagator, which is native-only.
pub const HEADER_NAME_TRACING_SPAN_CONTEXT: &str = "tracing-span-context";

/// Wrapper around headers map, mirroring the native injector shape.
///
/// On wasm there is no propagator, so this carries no behavior.
pub struct HeaderInjector<'a>(pub &'a mut HashMap<String, Bytes>);

/// Wrapper around headers map, mirroring the native extractor shape.
///
/// On wasm there is no propagator, so this carries no behavior.
pub struct HeaderExtractor<'a>(pub &'a HashMap<String, Bytes>);

/// Inject the current span's trace context into headers.
///
/// No-op on wasm: there is no OTLP backend to propagate to.
pub fn inject_trace_context(_headers: &mut HashMap<String, Bytes>) {}

/// Extract trace context from headers and set it as the current span's parent.
///
/// No-op on wasm: there is no OTLP backend to link to.
pub fn extract_trace_context(_headers: &HashMap<String, Bytes>) {}

/// Check if headers contain trace context.
pub fn has_trace_context(headers: &HashMap<String, Bytes>) -> bool {
    headers.contains_key("traceparent")
        || headers.contains_key("uber-trace-id")
        || headers.contains_key("x-b3-traceid")
        || headers.contains_key(HEADER_NAME_TRACING_SPAN_CONTEXT)
}

/// Create a span for a protocol operation.
///
/// On wasm no parent context is extracted, so this returns a plain span.
pub fn span_from_headers(
    protocol: &str,
    direction: &str,
    _headers: &HashMap<String, Bytes>,
) -> Span {
    tracing::info_span!("protocol", protocol = protocol, direction = direction,)
}

/// Create a span with peer identity context.
///
/// Like [`span_from_headers`] but adds `remote_peer_id` and `remote_overlay`
/// fields. On wasm no parent context is extracted.
pub(crate) fn span_from_headers_with_context(
    protocol: &str,
    direction: &str,
    _headers: &HashMap<String, Bytes>,
    ctx: &PeerContext,
) -> Span {
    tracing::info_span!(
        "protocol",
        protocol = protocol,
        direction = direction,
        remote_peer_id = %ctx.remote_peer_id,
        remote_overlay = %ctx.remote_overlay,
    )
}
