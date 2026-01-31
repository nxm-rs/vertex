use std::collections::HashMap;

use vertex_net_codec::{Codec, ProtoMessage, ProtocolCodecError};

use bytes::Bytes;

/// Error type for headers codec operations.
///
/// Headers has no domain-specific errors, so we use the base `ProtocolCodecError`.
pub type CodecError = ProtocolCodecError;

/// Codec for headers protocol messages.
pub type HeadersCodec = Codec<Headers, CodecError>;

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
    type DecodeError = CodecError;

    fn into_proto(self) -> Self::Proto {
        crate::proto::headers::Headers {
            headers: self
                .inner
                .into_iter()
                .map(|(k, v)| crate::proto::headers::Header {
                    key: k,
                    value: v.into(),
                })
                .collect(),
        }
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
