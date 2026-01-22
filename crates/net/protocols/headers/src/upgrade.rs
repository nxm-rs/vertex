//! Protocol upgrade wrappers that handle headers exchange.

use asynchronous_codec::Framed;
use futures::{future::BoxFuture, SinkExt, TryStreamExt};
use libp2p::{core::UpgradeInfo, InboundUpgrade, OutboundUpgrade, Stream};
use tracing::{debug, Instrument};

use crate::{
    codec::{Headers, HeadersCodec},
    error::{HeadersError, ProtocolError},
    stream::HeaderedStream,
    tracing::{inject_trace_context, span_from_headers},
    traits::{HeaderedInbound, HeaderedOutbound},
    MAX_HEADERS_SIZE,
};

/// Inbound wrapper - wraps `HeaderedInbound` into `InboundUpgrade<Stream>`.
///
/// Handles the headers exchange automatically:
/// 1. Reads peer's headers
/// 2. Calls `response_headers()` to compute our response (headler pattern)
/// 3. Sends our response headers
/// 4. Creates instrumented `HeaderedStream`
/// 5. Calls inner protocol's `read()`
#[derive(Debug, Clone)]
pub struct Inbound<P> {
    inner: P,
}

impl<P> Inbound<P> {
    /// Create a new inbound protocol wrapper.
    pub fn new(inner: P) -> Self {
        Self { inner }
    }
}

impl<P: HeaderedInbound> UpgradeInfo for Inbound<P> {
    type Info = &'static str;
    type InfoIter = std::iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once(self.inner.protocol_name())
    }
}

impl<P: HeaderedInbound> InboundUpgrade<Stream> for Inbound<P> {
    type Output = P::Output;
    type Error = ProtocolError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_inbound(self, socket: Stream, _info: Self::Info) -> Self::Future {
        let protocol_name = self.inner.protocol_name();

        Box::pin(async move {
            let codec = HeadersCodec::new(MAX_HEADERS_SIZE);
            let mut framed = Framed::new(socket, codec);

            // Phase 1: Read peer's headers
            debug!(protocol = protocol_name, "Reading peer headers");
            let peer_headers = framed
                .try_next()
                .await
                .map_err(HeadersError::from)?
                .ok_or(HeadersError::ConnectionClosed)?
                .into_inner();

            // Create tracing span from received headers (may contain remote trace context)
            let span = span_from_headers(protocol_name, "inbound", &peer_headers);

            // Run remaining work within the span
            async {
                // Phase 2: Compute and send response headers (headler pattern)
                let response_headers = self.inner.response_headers(&peer_headers);
                debug!(
                    protocol = protocol_name,
                    response_header_count = response_headers.len(),
                    "Sending response headers"
                );
                framed
                    .send(Headers::new(response_headers))
                    .await
                    .map_err(HeadersError::from)?;

                // Phase 3: Create HeaderedStream and call inner protocol
                let headered = HeaderedStream::new(framed.into_inner(), peer_headers);

                self.inner
                    .read(headered)
                    .await
                    .map_err(|e| ProtocolError::Protocol(e.into()))
            }
            .instrument(span)
            .await
        })
    }
}

/// Outbound wrapper - wraps `HeaderedOutbound` into `OutboundUpgrade<Stream>`.
///
/// Handles the headers exchange automatically:
/// 1. Calls `headers()` to get our headers to send
/// 2. Injects trace context into headers for distributed tracing
/// 3. Sends our headers
/// 4. Reads peer's response headers
/// 5. Creates instrumented `HeaderedStream`
/// 6. Calls inner protocol's `write()`
#[derive(Debug, Clone)]
pub struct Outbound<P> {
    inner: P,
}

impl<P> Outbound<P> {
    /// Create a new outbound protocol wrapper.
    pub fn new(inner: P) -> Self {
        Self { inner }
    }
}

impl<P: HeaderedOutbound> UpgradeInfo for Outbound<P> {
    type Info = &'static str;
    type InfoIter = std::iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once(self.inner.protocol_name())
    }
}

impl<P: HeaderedOutbound> OutboundUpgrade<Stream> for Outbound<P> {
    type Output = P::Output;
    type Error = ProtocolError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_outbound(self, socket: Stream, _info: Self::Info) -> Self::Future {
        let protocol_name = self.inner.protocol_name();

        Box::pin(async move {
            let codec = HeadersCodec::new(MAX_HEADERS_SIZE);
            let mut framed = Framed::new(socket, codec);

            // Phase 1: Get headers and inject trace context
            let mut our_headers = self.inner.headers();
            inject_trace_context(&mut our_headers);

            debug!(
                protocol = protocol_name,
                header_count = our_headers.len(),
                "Sending our headers"
            );
            framed
                .send(Headers::new(our_headers))
                .await
                .map_err(HeadersError::from)?;

            // Phase 2: Read peer's response headers
            debug!(protocol = protocol_name, "Reading peer response headers");
            let peer_headers = framed
                .try_next()
                .await
                .map_err(HeadersError::from)?
                .ok_or(HeadersError::ConnectionClosed)?
                .into_inner();

            // Create tracing span and run inner protocol within it
            let span = span_from_headers(protocol_name, "outbound", &peer_headers);

            async {
                let headered = HeaderedStream::new(framed.into_inner(), peer_headers);

                self.inner
                    .write(headered)
                    .await
                    .map_err(|e| ProtocolError::Protocol(e.into()))
            }
            .instrument(span)
            .await
        })
    }
}
