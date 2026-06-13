//! Codec for retrieval protocol messages.
//!
//! The retrieval `Delivery` wire frame is `data` + `stamp` + `err` with no
//! address (unlike pushsync, which carries one). Rebuilding the chunk from the
//! `data` bytes is ambiguous without the address, so the delivery decoder is
//! parameterized by the requested address, threaded in from the outbound side
//! that already knows it. The address is never put on the wire: a success
//! delivery is reconstructed and validated against the requested address, so a
//! mismatch is a decode error rather than a silently-wrong chunk.

use asynchronous_codec::{Decoder, Encoder};
use bytes::{Bytes, BytesMut};
use nectar_primitives::ChunkAddress;
use vertex_net_codec::{Codec, ProtoMessage};
use vertex_swarm_primitives::{Stamp, StampedChunk};

use crate::error::RetrievalError;

/// Codec for retrieval request messages.
pub(crate) type RequestCodec = Codec<Request, RetrievalError>;

/// A request for a chunk by its address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The address of the chunk to retrieve.
    pub address: ChunkAddress,
}

impl Request {
    /// Create a new retrieval request.
    pub fn new(address: ChunkAddress) -> Self {
        Self { address }
    }
}

impl ProtoMessage for Request {
    type Proto = vertex_swarm_net_proto::retrieval::Request;
    type EncodeError = std::convert::Infallible;
    type DecodeError = RetrievalError;

    fn into_proto(self) -> Result<Self::Proto, Self::EncodeError> {
        Ok(vertex_swarm_net_proto::retrieval::Request {
            addr: self.address.to_vec(),
        })
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        if proto.addr.len() != 32 {
            return Err(RetrievalError::InvalidAddressLength(proto.addr.len()));
        }
        let address = ChunkAddress::from_slice(&proto.addr)?;
        Ok(Self { address })
    }
}

/// Delivery of a chunk, or an opaque failure.
///
/// A successful delivery carries the [`StampedChunk`]. A failure carries no
/// payload: the remote's error string is adversarial input the reference does
/// not introspect, so we never read it. Failure is signalled on the wire by
/// empty `data`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// The chunk and its stamp.
    Chunk(Box<StampedChunk>),
    /// Retrieval failed. The reason is intentionally not carried.
    Error,
}

impl Delivery {
    /// Create a successful delivery.
    pub fn success(chunk: StampedChunk) -> Self {
        Self::Chunk(Box::new(chunk))
    }

    /// Create a failure delivery.
    pub fn error() -> Self {
        Self::Error
    }

    /// Check if this delivery is a failure.
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error)
    }

    /// Encode this delivery to its protobuf wire form. A failure is encoded as
    /// empty `data`/`stamp`; we emit nothing on the (omitted) error field.
    fn into_proto(self) -> vertex_swarm_net_proto::retrieval::Delivery {
        match self {
            Self::Chunk(chunk) => {
                let (chunk, stamp) = (*chunk).into_parts();
                vertex_swarm_net_proto::retrieval::Delivery {
                    data: chunk.into_bytes().to_vec(),
                    stamp: stamp.to_bytes().to_vec(),
                }
            }
            Self::Error => vertex_swarm_net_proto::retrieval::Delivery {
                data: Vec::new(),
                stamp: Vec::new(),
            },
        }
    }

    /// Decode a delivery from its protobuf wire form, reconstructing and
    /// validating the chunk against the requested address.
    ///
    /// An honest failure is detected structurally by an empty `data` AND an
    /// empty `stamp`: the reference reports a retrieval failure with an empty
    /// delivery (it does not reset the stream for retrieval), and that is the
    /// only signal we read. Any error string the remote set on the (unmodelled)
    /// wire field is skipped without allocation.
    ///
    /// Once `data` is non-empty the frame claims to carry a chunk, so it must
    /// reconstruct and validate against the requested address; otherwise it is
    /// malformed data, which surfaces as a decode error rather than collapsing
    /// into an honest-failure signal. The two are scored differently upstream,
    /// so the distinction is kept strict here.
    fn from_proto(
        proto: vertex_swarm_net_proto::retrieval::Delivery,
        expected: ChunkAddress,
    ) -> Result<Self, RetrievalError> {
        if proto.data.is_empty() && proto.stamp.is_empty() {
            return Ok(Self::Error);
        }
        let stamp = Stamp::try_from_slice(&proto.stamp)?;
        let chunk = StampedChunk::reconstruct(expected, Bytes::from(proto.data), stamp)?;
        Ok(Self::success(chunk))
    }
}

/// Codec for retrieval delivery messages.
///
/// Holds the requested chunk address so a decoded delivery can be reconstructed
/// and validated against it. Construct with [`new`](Self::new) on the outbound
/// (requester) side; the inbound (responder) side only encodes, so its address
/// is irrelevant.
pub(crate) struct DeliveryCodec {
    inner: quick_protobuf_codec::Codec<vertex_swarm_net_proto::retrieval::Delivery>,
    expected: ChunkAddress,
}

impl DeliveryCodec {
    /// Create a delivery codec that validates decoded chunks against `expected`.
    pub(crate) fn new(max_packet_size: usize, expected: ChunkAddress) -> Self {
        Self {
            inner: quick_protobuf_codec::Codec::new(max_packet_size),
            expected,
        }
    }
}

impl Encoder for DeliveryCodec {
    type Item<'a> = Delivery;
    type Error = RetrievalError;

    fn encode(&mut self, item: Self::Item<'_>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        self.inner
            .encode(item.into_proto(), dst)
            .map_err(Into::into)
    }
}

impl Decoder for DeliveryCodec {
    type Item = Delivery;
    type Error = RetrievalError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        match self.inner.decode(src)? {
            Some(proto) => Ok(Some(Delivery::from_proto(proto, self.expected)?)),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{B256, Signature};
    use nectar_primitives::{AnyChunk, ContentChunk, SingleOwnerChunk};
    use vertex_net_codec::assert_proto_roundtrip;

    /// A stamp with a deterministic, well-formed signature for roundtrip tests.
    fn test_stamp() -> Stamp {
        let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
        Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, sig)
    }

    fn content_stamped() -> StampedChunk {
        let chunk: AnyChunk = ContentChunk::new(&b"retrieval payload"[..])
            .expect("valid content chunk")
            .into();
        StampedChunk::new(chunk, test_stamp())
    }

    fn soc_stamped() -> StampedChunk {
        use alloy_signer_local::PrivateKeySigner;
        let signer = PrivateKeySigner::from_bytes(&B256::repeat_byte(0x11)).expect("valid signer");
        let chunk: AnyChunk =
            SingleOwnerChunk::new(B256::repeat_byte(0x22), &b"soc payload"[..], &signer)
                .expect("valid soc")
                .into();
        StampedChunk::new(chunk, test_stamp())
    }

    #[test]
    fn test_request_roundtrip() {
        assert_proto_roundtrip!(Request::new(ChunkAddress::new([0x42; 32])));
    }

    /// Encode a delivery and decode it back through the address-aware codec.
    fn roundtrip(stamped: StampedChunk) {
        let address = *stamped.address();
        let wire_data = stamped.chunk().clone().into_bytes();
        let mut enc = DeliveryCodec::new(1024 * 1024, address);
        let mut buf = BytesMut::new();
        enc.encode(Delivery::success(stamped), &mut buf).unwrap();

        let mut dec = DeliveryCodec::new(1024 * 1024, address);
        let decoded = dec.decode(&mut buf).unwrap().expect("frame decoded");
        match decoded {
            Delivery::Chunk(chunk) => {
                assert_eq!(*chunk.address(), address);
                assert_eq!(chunk.chunk().clone().into_bytes(), wire_data);
            }
            Delivery::Error => panic!("expected chunk, got error"),
        }
    }

    #[test]
    fn test_delivery_content_roundtrip() {
        roundtrip(content_stamped());
    }

    #[test]
    fn test_delivery_soc_roundtrip() {
        roundtrip(soc_stamped());
    }

    #[test]
    fn test_delivery_error_roundtrip() {
        let address = ChunkAddress::new([0x42; 32]);
        let mut enc = DeliveryCodec::new(1024, address);
        let mut buf = BytesMut::new();
        enc.encode(Delivery::error(), &mut buf).unwrap();

        let mut dec = DeliveryCodec::new(1024, address);
        let decoded = dec.decode(&mut buf).unwrap().expect("frame decoded");
        assert!(decoded.is_error());
        assert_eq!(decoded, Delivery::error());
    }

    /// Wire-compat: the reference signals a retrieval failure with empty data
    /// plus a (now unmodelled) `err` string on field 3. We must decode that as a
    /// failure and skip the err bytes entirely. This hand-builds such a frame:
    /// a length-delimited message carrying only field 3 (`tag 0x1A`, wire type 2)
    /// with an arbitrary error string, and asserts the decoder ignores it.
    #[test]
    fn decodes_reference_failure_frame_ignoring_err_field() {
        let address = ChunkAddress::new([0x42; 32]);
        // Inner message: field 3 (err), length-delimited, value "boom". No data,
        // no stamp (both empty, hence omitted on the wire).
        let inner = [&[0x1Au8, 0x04][..], b"boom"].concat();
        // quick-protobuf framing prepends the message length as a varint; for a
        // 6-byte message that is a single byte.
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&[inner.len() as u8]);
        buf.extend_from_slice(&inner);

        let mut dec = DeliveryCodec::new(1024, address);
        let decoded = dec.decode(&mut buf).unwrap().expect("frame decoded");
        assert!(decoded.is_error(), "empty data must decode as a failure");
        assert_eq!(decoded, Delivery::error());
    }

    #[test]
    fn test_delivery_rejects_wrong_address() {
        let stamped = content_stamped();
        let mut enc = DeliveryCodec::new(1024 * 1024, *stamped.address());
        let mut buf = BytesMut::new();
        enc.encode(Delivery::success(stamped), &mut buf).unwrap();

        // Decode with the wrong expected address: reconstruction must fail.
        let mut dec = DeliveryCodec::new(1024 * 1024, ChunkAddress::new([0xff; 32]));
        let err = dec.decode(&mut buf).expect_err("wrong address must fail");
        assert!(matches!(err, RetrievalError::InvalidChunk(_)));
    }
}
