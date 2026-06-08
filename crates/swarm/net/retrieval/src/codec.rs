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

/// Delivery of a chunk, or a not-found error.
///
/// A successful delivery carries the [`StampedChunk`]; a failed one carries the
/// remote's error string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// The chunk and its stamp.
    ///
    /// Boxed: a [`StampedChunk`] is far larger than the error string, so boxing
    /// keeps the enum small for the common error path.
    Chunk(Box<StampedChunk>),
    /// Retrieval failed (e.g. chunk not found), with the remote's message.
    Error(String),
}

impl Delivery {
    /// Create a successful delivery.
    pub fn success(chunk: StampedChunk) -> Self {
        Self::Chunk(Box::new(chunk))
    }

    /// Create an error delivery.
    pub fn error(msg: impl Into<String>) -> Self {
        Self::Error(msg.into())
    }

    /// Check if this delivery is an error.
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error(_))
    }

    /// Encode this delivery to its protobuf wire form.
    fn into_proto(self) -> vertex_swarm_net_proto::retrieval::Delivery {
        match self {
            Self::Chunk(chunk) => {
                let (chunk, stamp) = (*chunk).into_parts();
                vertex_swarm_net_proto::retrieval::Delivery {
                    data: chunk.into_bytes().to_vec(),
                    stamp: stamp.to_bytes().to_vec(),
                    err: String::new(),
                }
            }
            Self::Error(err) => vertex_swarm_net_proto::retrieval::Delivery {
                data: Vec::new(),
                stamp: Vec::new(),
                err,
            },
        }
    }

    /// Decode a delivery from its protobuf wire form, reconstructing and
    /// validating the chunk against the requested address.
    ///
    /// An error delivery carries the `err` string and no stamp; it is decodable
    /// from `err` alone, so the chunk is not reconstructed in that case.
    fn from_proto(
        proto: vertex_swarm_net_proto::retrieval::Delivery,
        expected: ChunkAddress,
    ) -> Result<Self, RetrievalError> {
        if !proto.err.is_empty() {
            return Ok(Self::error(proto.err));
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
            Delivery::Error(e) => panic!("expected chunk, got error {e}"),
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
        enc.encode(Delivery::error("chunk not found"), &mut buf)
            .unwrap();

        let mut dec = DeliveryCodec::new(1024, address);
        let decoded = dec.decode(&mut buf).unwrap().expect("frame decoded");
        assert!(decoded.is_error());
        assert_eq!(decoded, Delivery::error("chunk not found"));
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
