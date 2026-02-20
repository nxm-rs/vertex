//! Protocol headers for Swarm P2P communication with distributed tracing.

mod codec;
mod error;
mod stream;
mod tracing;
mod traits;
mod upgrade;

// Re-exports
pub use codec::{Headers, HeadersCodec};
pub use error::{HeadersError, ProtocolError, UpgradeError};
pub use stream::HeaderedStream;
pub use tracing::{
    HEADER_NAME_TRACING_SPAN_CONTEXT, HeaderExtractor, HeaderInjector, extract_trace_context,
    has_trace_context, inject_trace_context, span_from_headers,
};
pub use traits::{HeaderedInbound, HeaderedOutbound};
pub use upgrade::{Inbound, Outbound};

/// Maximum size of headers message in bytes.
pub const MAX_HEADERS_SIZE: usize = 1024;
