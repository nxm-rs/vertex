use bytes::Bytes;
use futures::AsyncRead;
use futures::AsyncWrite;
use libp2p::Stream;
use std::collections::HashMap;
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tracing::Span;

/// A wrapper around a libp2p Stream that maintains tracing instrumentation
/// with protocol headers as span fields.
pub struct InstrumentedStream {
    /// The underlying network stream
    stream: Stream,
    /// Protocol headers exchanged during upgrade
    headers: HashMap<String, Bytes>,
    /// Keeps the span entered for the lifetime of the stream
    _span_guard: tracing::span::EnteredSpan,
}

impl InstrumentedStream {
    /// Create a new instrumented stream with automatic span management
    pub fn new(stream: Stream, headers: HashMap<String, Bytes>, span: Span) -> Self {
        Self {
            stream,
            headers,
            _span_guard: span.entered(),
        }
    }

    /// Get a reference to the protocol headers
    pub fn headers(&self) -> &HashMap<String, Bytes> {
        &self.headers
    }

    /// Get a specific header value
    pub fn get_header(&self, key: &str) -> Option<&Bytes> {
        self.headers.get(key)
    }

    /// Get the underlying stream and headers, consuming this wrapper
    pub fn into_inner(self) -> (Stream, HashMap<String, Bytes>) {
        (self.stream, self.headers)
    }
}

impl AsyncRead for InstrumentedStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for InstrumentedStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_close(cx)
    }
}

// Add AsRef/AsMut implementations for easier access to the underlying stream
impl AsRef<Stream> for InstrumentedStream {
    fn as_ref(&self) -> &Stream {
        &self.stream
    }
}

impl AsMut<Stream> for InstrumentedStream {
    fn as_mut(&mut self) -> &mut Stream {
        &mut self.stream
    }
}
