//! Protocol headers for Swarm P2P communication with distributed tracing.

mod codec;
mod error;
pub mod metrics;
mod stream;
// Trace-context propagation has a native implementation over OpenTelemetry and a
// no-op wasm sibling (the browser client has no OTLP backend). Both export the
// same item names with identical signatures so `upgrade.rs` compiles unchanged.
#[cfg_attr(target_arch = "wasm32", path = "tracing_wasm.rs")]
mod tracing;
mod traits;
mod upgrade;

// Re-exports
pub use codec::{Headers, HeadersCodec};
pub use error::{HeadersError, ProtocolError, ProtocolStreamError, UpgradeError};
pub use stream::HeaderedStream;
pub use tracing::{
    HEADER_NAME_TRACING_SPAN_CONTEXT, HeaderExtractor, HeaderInjector, PeerContext,
    extract_trace_context, has_trace_context, inject_trace_context, span_from_headers,
};
pub use traits::{HeaderedInbound, HeaderedOutbound};
pub use upgrade::{Inbound, Outbound};

/// Maximum size of headers message in bytes.
pub const MAX_HEADERS_SIZE: usize = 1024;
