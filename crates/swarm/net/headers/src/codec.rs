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
mod tests {
    use super::*;
    use crate::fuzz;

    /// Replays the committed fuzz seeds through the exact decode path the
    /// `headers_decode` fuzz target drives, so the stable test gate proves
    /// the seeds stay panic-free without the fuzzer. Every committed seed
    /// carries well-formed nesting, so the raw generated reader is safe on
    /// the envelope frame.
    #[test]
    fn seed_replay_headers_decode() {
        let seed_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../fuzz/seeds/headers_decode");
        let mut replayed = 0usize;
        for entry in std::fs::read_dir(&seed_dir)
            .unwrap_or_else(|e| panic!("seed dir {} must exist: {e}", seed_dir.display()))
        {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let data = std::fs::read(&path).unwrap();

            let headers = fuzz::decode_headers(&data);
            if name.starts_with("valid-") || name.starts_with("edge-") {
                let decoded = headers.unwrap_or_else(|| panic!("seed {name} must decode"));
                // The decoded map must survive the shared invariant check.
                let wire = decoded.into_proto().unwrap();
                fuzz::check_headers(wire);
            } else if name.starts_with("invalid-") {
                assert!(headers.is_none(), "seed {name} must stay an Err");
            }
            replayed += 1;
        }
        assert!(
            replayed >= 6,
            "expected at least the 6 curated seeds, found {replayed}"
        );
    }
}

// Stable pins of the round-trip invariant, drawing the envelope through the
// shared `Arbitrary` impl so the proptest suite and the fuzz round-trip
// target drive one construction path. Keys are unique by map construction,
// so decode's collect loses nothing.
#[cfg(test)]
mod proptests {
    use proptest::prelude::*;
    use proptest_arbitrary_interop::arb;
    use vertex_net_codec::prop_assert_proto_roundtrip;

    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn headers_roundtrip(headers in arb::<Headers>()) {
            prop_assert_proto_roundtrip!(headers);
        }
    }
}
