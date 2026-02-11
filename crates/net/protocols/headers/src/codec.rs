//! Codec for headers protocol messages.

use std::collections::HashMap;

use bytes::Bytes;
use vertex_net_codec::{Codec, ProtoMessage};

use crate::error::HeadersError;

/// Codec for headers protocol messages.
pub type HeadersCodec = Codec<Headers, HeadersError>;

/// Headers message wrapper.
#[derive(Debug, Clone, PartialEq)]
pub struct Headers {
    inner: HashMap<String, Bytes>,
}

impl Headers {
    /// Create a new Headers message.
    pub fn new(inner: HashMap<String, Bytes>) -> Self {
        Headers { inner }
    }

    /// Get the inner headers map.
    pub fn into_inner(self) -> HashMap<String, Bytes> {
        self.inner
    }
}

impl ProtoMessage for Headers {
    type Proto = crate::proto::headers::Headers;
    type EncodeError = std::convert::Infallible;
    type DecodeError = HeadersError;

    fn into_proto(self) -> Result<Self::Proto, Self::EncodeError> {
        Ok(crate::proto::headers::Headers {
            headers: self
                .inner
                .into_iter()
                .map(|(k, v)| crate::proto::headers::Header {
                    key: k,
                    value: v.into(),
                })
                .collect(),
        })
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        Ok(Headers {
            inner: proto
                .headers
                .into_iter()
                .map(|v| (v.key, Bytes::from(v.value)))
                .collect(),
        })
    }
}
