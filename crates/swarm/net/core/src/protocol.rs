//! The [`SwarmProtocol`] trait — one shape for every Swarm wire protocol.

use libp2p::StreamProtocol;

use crate::SemanticVersion;

/// A protobuf codec for a Swarm protocol message.
///
/// This is intentionally a minimal marker trait: it associates a codec type
/// with the domain message it encodes, without prescribing how that codec is
/// implemented. Concrete codecs (built on `quick-protobuf-codec`, `prost`, or
/// the `vertex-net-codec` framed wrapper) live in their respective protocol
/// crates and implement this trait to plug into the [`SwarmProtocol`] shape.
pub trait ProtoCodec {
    /// The decoded domain message this codec produces.
    type Message;
}

/// One shape for every Swarm wire protocol.
///
/// Implementors describe themselves via compile-time metadata:
///
/// - [`NAME`](Self::NAME) — the second segment of the protocol ID
///   (`/swarm/{NAME}/.../...`),
/// - [`VERSION`](Self::VERSION) — the semantic version segment,
/// - [`STREAM_NAME`](Self::STREAM_NAME) — the trailing segment that
///   distinguishes streams that share a protocol name (e.g. `hive`'s `peers`),
/// - [`Codec`](Self::Codec) — the wire codec, and
/// - [`Message`](Self::Message) — the domain message it produces.
///
/// The full libp2p `StreamProtocol` ID is composed from those parts by
/// [`full_protocol_id`](Self::full_protocol_id), guaranteeing every Swarm
/// protocol agrees on the `/swarm/{name}/{version}/{stream}` layout.
///
/// This trait is metadata-only: implementing it does not change runtime
/// behaviour, dispatch, or the wire format of any existing protocol.
pub trait SwarmProtocol {
    /// Protocol family name, e.g. `"handshake"`, `"hive"`, `"pingpong"`.
    const NAME: &'static str;

    /// Semantic version of this protocol.
    const VERSION: SemanticVersion;

    /// Trailing stream segment of the protocol ID. Usually equal to
    /// [`NAME`](Self::NAME), but some families (e.g. `hive`) use a distinct
    /// stream name such as `"peers"`.
    const STREAM_NAME: &'static str;

    /// Wire codec used to frame [`Message`](Self::Message) on this protocol.
    type Codec: ProtoCodec<Message = Self::Message>;

    /// Decoded domain message this protocol exchanges.
    type Message;

    /// Compose the full `/swarm/{name}/{version}/{stream}` libp2p protocol ID.
    fn full_protocol_id() -> StreamProtocol;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyCodec;

    impl ProtoCodec for DummyCodec {
        type Message = u32;
    }

    struct Dummy;

    impl SwarmProtocol for Dummy {
        const NAME: &'static str = "dummy";
        const VERSION: SemanticVersion = SemanticVersion::new(2, 3, 4);
        const STREAM_NAME: &'static str = "stream";
        type Codec = DummyCodec;
        type Message = u32;

        fn full_protocol_id() -> StreamProtocol {
            StreamProtocol::new("/swarm/dummy/2.3.4/stream")
        }
    }

    #[test]
    fn metadata_round_trip() {
        assert_eq!(Dummy::NAME, "dummy");
        assert_eq!(Dummy::STREAM_NAME, "stream");
        assert_eq!(Dummy::VERSION, SemanticVersion::new(2, 3, 4));
        assert_eq!(
            Dummy::full_protocol_id().as_ref(),
            "/swarm/dummy/2.3.4/stream"
        );
    }
}
