use std::collections::HashMap;

use vertex_net_codec::ProtocolCodec;

use bytes::Bytes;

/// Codec for headers protocol messages.
pub type HeadersCodec = ProtocolCodec<crate::proto::headers::Headers, Headers, CodecError>;

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("Protocol error: {0}")]
    Protocol(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<quick_protobuf_codec::Error> for CodecError {
    fn from(error: quick_protobuf_codec::Error) -> Self {
        CodecError::Protocol(error.to_string())
    }
}

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

impl TryFrom<crate::proto::headers::Headers> for Headers {
    type Error = CodecError;

    fn try_from(value: crate::proto::headers::Headers) -> Result<Self, Self::Error> {
        Ok(Headers {
            inner: value
                .headers
                .into_iter()
                .map(|v| (v.key, Bytes::from(v.value)))
                .collect(),
        })
    }
}

impl Into<crate::proto::headers::Headers> for Headers {
    fn into(self) -> crate::proto::headers::Headers {
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
}
