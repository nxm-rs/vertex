//! Protocol headers for Swarm peer-to-peer communication.
//!
//! All Swarm protocols (except handshake) exchange headers before protocol data.
//! Headers serve two purposes:
//!
//! 1. **Tracing** - Propagating distributed tracing span context across peers
//! 2. **Protocol negotiation** - e.g., SWAP protocol exchanges exchange rates
//!
//! # Usage
//!
//! Implement [`HeaderedInbound`] or [`HeaderedOutbound`] for your protocol,
//! then wrap with [`Inbound`] or [`Outbound`] to get `InboundUpgrade`/`OutboundUpgrade`.
//!
//! ```ignore
//! use vertex_net_headers::{HeaderedInbound, HeaderedStream, Inbound};
//!
//! struct MyProtocol;
//!
//! impl HeaderedInbound for MyProtocol {
//!     type Output = MyData;
//!     type Error = MyError;
//!
//!     fn protocol_name(&self) -> &'static str {
//!         "/swarm/myproto/1.0.0/stream"
//!     }
//!
//!     // Optional: compute response headers based on received headers (headler pattern)
//!     fn response_headers(&self, peer_headers: &HashMap<String, Bytes>) -> HashMap<String, Bytes> {
//!         // ... negotiate based on peer_headers
//!         HashMap::new()
//!     }
//!
//!     fn read(self, stream: HeaderedStream) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
//!         Box::pin(async move {
//!             // Read protocol data from stream.into_inner()
//!             todo!()
//!         })
//!     }
//! }
//!
//! // Use in handler:
//! type InboundProtocol = Inbound<MyProtocol>;
//! ```
//!
//! # Distributed Tracing
//!
//! The headers module automatically propagates trace context:
//!
//! - **Outbound**: Injects trace context into headers via [`inject_trace_context`]
//! - **Inbound**: Extracts trace context and creates a correlated span via [`span_from_headers`]
//!
//! This enables distributed tracing across the Swarm network when using
//! a compatible tracing subscriber.

// Internal modules
#[allow(unreachable_pub)]
mod proto {
    include!(concat!(env!("OUT_DIR"), "/proto/mod.rs"));
}

mod codec;
mod error;
mod stream;
mod tracing;
mod traits;
mod upgrade;

// Re-exports
pub use codec::{CodecError, Headers, HeadersCodec};
pub use error::{HeadersError, ProtocolError};
pub use stream::HeaderedStream;
pub use tracing::{
    HEADER_NAME_TRACING_SPAN_CONTEXT, HeaderExtractor, HeaderInjector, extract_trace_context,
    has_trace_context, inject_trace_context, span_from_headers,
};
pub use traits::{HeaderedInbound, HeaderedOutbound};
pub use upgrade::{Inbound, Outbound};

/// Maximum size of headers message in bytes.
pub const MAX_HEADERS_SIZE: usize = 1024;
