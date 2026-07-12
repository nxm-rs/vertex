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
    type Proto = vertex_swarm_net_proto::headers::Headers;
    type EncodeError = std::convert::Infallible;
    type DecodeError = HeadersError;

    fn into_proto(self) -> Result<Self::Proto, Self::EncodeError> {
        Ok(vertex_swarm_net_proto::headers::Headers {
            headers: self
                .inner
                .into_iter()
                .map(|(k, v)| vertex_swarm_net_proto::headers::Header {
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

#[cfg(test)]
mod proptests {
    use proptest::prelude::*;
    use vertex_net_codec::prop_assert_proto_roundtrip;

    use super::*;

    // Keys are unique by map construction, so decode's collect loses nothing.
    fn headers_map() -> impl Strategy<Value = HashMap<String, Bytes>> {
        prop::collection::hash_map(
            any::<String>(),
            prop::collection::vec(any::<u8>(), 0..=32).prop_map(Bytes::from),
            0..=8,
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn headers_roundtrip(map in headers_map()) {
            prop_assert_proto_roundtrip!(Headers::new(map));
        }
    }
}
