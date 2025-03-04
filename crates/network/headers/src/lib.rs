use std::collections::HashMap;

use asynchronous_codec::Framed;
use bytes::Bytes;
use futures::{future::BoxFuture, SinkExt, TryStreamExt};
use libp2p::{core::UpgradeInfo, InboundUpgrade, OutboundUpgrade, Stream};

mod proto {
    include!(concat!(env!("OUT_DIR"), "/proto/mod.rs"));
}

mod codec;
use codec::{CodecError, Headers, HeadersCodec};
mod instrumented;
use instrumented::InstrumentedStream;
use tracing::{debug, info_span, Span};

pub type HeadersFn = dyn Fn(HashMap<String, Bytes>) -> HashMap<String, Bytes> + Send + Sync;

pub struct HeaderedProtocol {
    protocols: &'static [&'static str],
    headers: HashMap<String, Bytes>,
    response_headers_fn: Box<HeadersFn>,
}

#[derive(Debug, thiserror::Error)]
pub enum HeadersError {
    #[error("Codec error: {0}")]
    Codec(#[from] CodecError),
    #[error("Connection closed")]
    ConnectionClosed,
}

impl HeaderedProtocol {
    pub const fn new(
        protocols: &'static [&'static str],
        headers: HashMap<String, Bytes>,
        response_headers_fn: Box<HeadersFn>,
    ) -> Self {
        Self {
            protocols,
            headers,
            response_headers_fn,
        }
    }

    pub fn headers(&self) -> &HashMap<String, Bytes> {
        &self.headers
    }

    async fn handle_inbound_headers(
        &self,
        stream: Stream,
    ) -> Result<(Stream, HashMap<String, Bytes>), HeadersError> {
        // Set up codecs
        let headers_codec = HeadersCodec::new(1024);

        // Read HEADERS using framed read
        let mut framed = Framed::new(stream, headers_codec);
        debug!("Attempting to read headers");
        let headers = framed
            .try_next()
            .await?
            .ok_or(HeadersError::ConnectionClosed)?
            .into_inner();

        // Given the headers, generate a response
        let response_headers = (self.response_headers_fn)(headers.clone());

        // Send response headers
        framed.send(Headers::new(response_headers)).await?;

        Ok((framed.into_inner(), headers))
    }

    async fn handle_outbound_headers(
        &self,
        socket: Stream,
    ) -> Result<(Stream, HashMap<String, Bytes>), HeadersError> {
        // Set up codecs
        let headers_codec = HeadersCodec::new(1024);
        let mut framed = Framed::new(socket, headers_codec);

        // Send HEADERS using framed send
        framed.send(Headers::new(self.headers.clone())).await?;

        // Read response headers
        debug!("Attempting to read headers");
        let headers = framed
            .try_next()
            .await?
            .ok_or(HeadersError::ConnectionClosed)?
            .into_inner();

        Ok((framed.into_inner(), headers))
    }
}

impl UpgradeInfo for HeaderedProtocol {
    type Info = &'static str;
    type InfoIter = std::iter::Copied<std::slice::Iter<'static, &'static str>>;

    fn protocol_info(&self) -> Self::InfoIter {
        self.protocols.iter().copied()
    }
}

impl InboundUpgrade<Stream> for HeaderedProtocol {
    type Output = InstrumentedStream;
    type Error = HeadersError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_inbound(self, socket: Stream, info: Self::Info) -> Self::Future {
        Box::pin(async move {
            let (stream, headers) = self.handle_inbound_headers(socket).await?;
            let span = create_headers_span(info, "inbound", &headers);
            Ok(InstrumentedStream::new(stream, headers, span))
        })
    }
}

impl OutboundUpgrade<Stream> for HeaderedProtocol {
    type Output = InstrumentedStream;
    type Error = HeadersError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_outbound(self, socket: Stream, info: Self::Info) -> Self::Future {
        Box::pin(async move {
            let (stream, headers) = self.handle_outbound_headers(socket).await?;
            let span = create_headers_span(info, "outbound", &headers);
            Ok(InstrumentedStream::new(stream, headers, span))
        })
    }
}

// Helper function to create span with headers
pub fn create_headers_span(
    protocol: &str,
    direction: &str,
    headers: &HashMap<String, Bytes>,
) -> Span {
    let span = info_span!("headers", ?protocol, ?direction);

    for (key, value) in headers {
        if let Ok(value_str) = String::from_utf8(value.to_vec()) {
            span.record(key.as_str(), &value_str.as_str());
        } else {
            span.record(key.as_str(), &format!("{:?}", value).as_str());
        }
    }

    span
}
