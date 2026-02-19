//! HeaderedStream - a stream that has completed headers exchange.

use std::collections::HashMap;

use bytes::Bytes;
use libp2p::Stream;

/// A stream that has completed headers exchange.
///
/// Protocols receive this instead of raw `Stream`, ensuring headers can't be forgotten.
/// The stream is already instrumented with a tracing span - all work done with this
/// stream will be attributed to that span.
pub struct HeaderedStream {
    inner: Stream,
    headers: HashMap<String, Bytes>,
}

impl HeaderedStream {
    /// Create a new HeaderedStream.
    pub(crate) fn new(inner: Stream, headers: HashMap<String, Bytes>) -> Self {
        Self { inner, headers }
    }

    /// Get the received headers.
    pub fn headers(&self) -> &HashMap<String, Bytes> {
        &self.headers
    }

    /// Consume and return the underlying stream.
    pub fn into_inner(self) -> Stream {
        self.inner
    }
}
