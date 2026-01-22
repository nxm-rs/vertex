//! Traits for headered protocols.

use std::collections::HashMap;

use bytes::Bytes;
use futures::future::BoxFuture;

use crate::HeaderedStream;

/// Trait for protocols that read data after headers (inbound/receiving side).
///
/// Implement this instead of `InboundUpgrade<Stream>` to ensure headers are handled.
/// The wrapper `Inbound<P>` will handle the headers exchange automatically.
///
/// # Example
///
/// ```ignore
/// impl HeaderedInbound for MyProtocol {
///     type Output = MyData;
///     type Error = MyError;
///
///     fn protocol_name(&self) -> &'static str {
///         "/swarm/myproto/1.0.0/stream"
///     }
///
///     fn read(self, stream: HeaderedStream) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
///         Box::pin(async move {
///             // Read and process data from stream.into_inner()
///         })
///     }
/// }
/// ```
pub trait HeaderedInbound: Send + 'static {
    type Output: Send + 'static;
    type Error: Into<Box<dyn std::error::Error + Send + Sync>>;

    /// The protocol name (e.g., "/swarm/hive/1.1.0/peers").
    fn protocol_name(&self) -> &'static str;

    /// Compute response headers based on received peer headers.
    ///
    /// This is the "headler" pattern from Bee - allows protocols to negotiate
    /// parameters (like exchange rates in SWAP) based on the peer's headers.
    ///
    /// Default implementation returns empty headers.
    fn response_headers(&self, _peer_headers: &HashMap<String, Bytes>) -> HashMap<String, Bytes> {
        HashMap::new()
    }

    /// Read and process protocol data from the stream.
    ///
    /// Called after headers exchange is complete. The stream's headers contain
    /// what the peer sent us.
    fn read(self, stream: HeaderedStream) -> BoxFuture<'static, Result<Self::Output, Self::Error>>;
}

/// Trait for protocols that write data after headers (outbound/sending side).
///
/// Implement this instead of `OutboundUpgrade<Stream>` to ensure headers are handled.
/// The wrapper `Outbound<P>` will handle the headers exchange automatically.
///
/// # Example
///
/// ```ignore
/// impl HeaderedOutbound for MyProtocol {
///     type Output = ();
///     type Error = MyError;
///
///     fn protocol_name(&self) -> &'static str {
///         "/swarm/myproto/1.0.0/stream"
///     }
///
///     fn write(self, stream: HeaderedStream) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
///         Box::pin(async move {
///             // Write data to stream.into_inner()
///         })
///     }
/// }
/// ```
pub trait HeaderedOutbound: Send + 'static {
    type Output: Send + 'static;
    type Error: Into<Box<dyn std::error::Error + Send + Sync>>;

    /// The protocol name (e.g., "/swarm/hive/1.1.0/peers").
    fn protocol_name(&self) -> &'static str;

    /// Headers to send to the peer.
    ///
    /// Default implementation returns empty headers.
    fn headers(&self) -> HashMap<String, Bytes> {
        HashMap::new()
    }

    /// Write protocol data to the stream.
    ///
    /// Called after headers exchange is complete. The stream's headers contain
    /// what the peer sent us in response.
    fn write(self, stream: HeaderedStream) -> BoxFuture<'static, Result<Self::Output, Self::Error>>;
}
