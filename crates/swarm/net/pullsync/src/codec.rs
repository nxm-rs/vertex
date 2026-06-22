//! Codec for pullsync protocol messages.
//!
//! Every domain type here converts to and from a `vertex_swarm_net_proto::pullsync`
//! message at the [`ProtoMessage`] boundary; the protobuf type never escapes
//! this module.

use alloy_primitives::B256;
use bytes::Bytes;
use nectar_primitives::ChunkAddress;
use vertex_net_codec::{Codec, ProtoMessage};
use vertex_swarm_primitives::{BatchId, Bin, StampedChunk};

use crate::bitvector::BitVector;
use crate::error::PullsyncError;

pub(crate) type SynCodec = Codec<Syn, PullsyncError>;
pub(crate) type AckCodec = Codec<Ack, PullsyncError>;
pub(crate) type GetCodec = Codec<Get, PullsyncError>;
pub(crate) type OfferCodec = Codec<Offer, PullsyncError>;
pub(crate) type WantCodec = Codec<Want, PullsyncError>;
pub(crate) type DeliveryCodec = Codec<Delivery, PullsyncError>;

/// Open the cursor handshake. Carries no payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Syn;

impl ProtoMessage for Syn {
    type Proto = vertex_swarm_net_proto::pullsync::Syn;
    type EncodeError = std::convert::Infallible;
    type DecodeError = PullsyncError;

    fn into_proto(self) -> Result<Self::Proto, Self::EncodeError> {
        Ok(vertex_swarm_net_proto::pullsync::Syn {})
    }

    fn from_proto(_proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        Ok(Self)
    }
}

/// Cursor handshake reply. `cursors[bin]` is the topmost id the responder holds
/// for that bin (`0` when empty); `epoch` is its reserve generation marker, so a
/// requester can detect a reserve reset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ack {
    pub cursors: Vec<u64>,
    pub epoch: u64,
}

impl ProtoMessage for Ack {
    type Proto = vertex_swarm_net_proto::pullsync::Ack;
    type EncodeError = std::convert::Infallible;
    type DecodeError = PullsyncError;

    fn into_proto(self) -> Result<Self::Proto, Self::EncodeError> {
        Ok(vertex_swarm_net_proto::pullsync::Ack {
            cursors: self.cursors,
            epoch: self.epoch,
        })
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        Ok(Self {
            cursors: proto.cursors,
            epoch: proto.epoch,
        })
    }
}

/// Request the chunks in `bin` from bin id `start` (inclusive) upward. The wire
/// `bin` field is `int32`; a value outside `0..=MAX_PO` is a decode error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Get {
    pub bin: Bin,
    pub start: u64,
}

impl Get {
    pub fn new(bin: Bin, start: u64) -> Self {
        Self { bin, start }
    }
}

impl ProtoMessage for Get {
    type Proto = vertex_swarm_net_proto::pullsync::Get;
    type EncodeError = std::convert::Infallible;
    type DecodeError = PullsyncError;

    fn into_proto(self) -> Result<Self::Proto, Self::EncodeError> {
        Ok(vertex_swarm_net_proto::pullsync::Get {
            bin: i32::from(self.bin.get()),
            start: self.start,
        })
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        let raw = u8::try_from(proto.bin).map_err(|_| PullsyncError::InvalidBin(proto.bin))?;
        let bin = Bin::new(raw).map_err(|_| PullsyncError::InvalidBin(proto.bin))?;
        Ok(Self {
            bin,
            start: proto.start,
        })
    }
}

/// One chunk advertised in an [`Offer`]: 32B address, 32B batch id, 32B stamp
/// hash. No chunk data, only the identity a requester needs to want it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkDescriptor {
    pub address: ChunkAddress,
    pub batch_id: BatchId,
    pub stamp_hash: B256,
}

impl ChunkDescriptor {
    pub fn new(address: ChunkAddress, batch_id: BatchId, stamp_hash: B256) -> Self {
        Self {
            address,
            batch_id,
            stamp_hash,
        }
    }

    fn into_proto(self) -> vertex_swarm_net_proto::pullsync::Chunk {
        vertex_swarm_net_proto::pullsync::Chunk {
            address: self.address.to_vec(),
            batch_id: self.batch_id.to_vec(),
            stamp_hash: self.stamp_hash.to_vec(),
        }
    }

    fn from_proto(proto: vertex_swarm_net_proto::pullsync::Chunk) -> Result<Self, PullsyncError> {
        let address = ChunkAddress::from_slice(&proto.address)?;
        let batch_id = b256_from_slice(&proto.batch_id, "batch_id")?;
        let stamp_hash = b256_from_slice(&proto.stamp_hash, "stamp_hash")?;
        Ok(Self {
            address,
            batch_id,
            stamp_hash,
        })
    }
}

/// Decode a 32-byte field, reporting which field was the wrong length.
fn b256_from_slice(slice: &[u8], field: &'static str) -> Result<B256, PullsyncError> {
    B256::try_from(slice).map_err(|_| PullsyncError::InvalidFieldLength {
        field,
        len: slice.len(),
    })
}

/// The responder's answer to a [`Get`]: descriptors for the range, up to
/// [`DEFAULT_MAX_PAGE`](crate::DEFAULT_MAX_PAGE) per page. `topmost` is the
/// highest bin id covered, so the requester advances its cursor even when it
/// wants none of the offered chunks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub topmost: u64,
    pub chunks: Vec<ChunkDescriptor>,
}

impl Offer {
    pub fn new(topmost: u64, chunks: Vec<ChunkDescriptor>) -> Self {
        Self { topmost, chunks }
    }
}

impl ProtoMessage for Offer {
    type Proto = vertex_swarm_net_proto::pullsync::Offer;
    type EncodeError = std::convert::Infallible;
    type DecodeError = PullsyncError;

    fn into_proto(self) -> Result<Self::Proto, Self::EncodeError> {
        Ok(vertex_swarm_net_proto::pullsync::Offer {
            topmost: self.topmost,
            chunks: self
                .chunks
                .into_iter()
                .map(ChunkDescriptor::into_proto)
                .collect(),
        })
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        let chunks = proto
            .chunks
            .into_iter()
            .map(ChunkDescriptor::from_proto)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            topmost: proto.topmost,
            chunks,
        })
    }
}

/// Selection reply to an [`Offer`]: bit `i` set means the requester wants
/// `chunks[i]`. The responder then sends one [`Delivery`] per set bit, in offer
/// order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Want {
    pub wanted: BitVector,
}

impl Want {
    pub fn new(wanted: BitVector) -> Self {
        Self { wanted }
    }

    /// The number of deliveries this want asks for.
    #[must_use]
    pub fn count(&self) -> usize {
        self.wanted.count_ones()
    }
}

impl ProtoMessage for Want {
    type Proto = vertex_swarm_net_proto::pullsync::Want;
    type EncodeError = std::convert::Infallible;
    type DecodeError = PullsyncError;

    fn into_proto(self) -> Result<Self::Proto, Self::EncodeError> {
        Ok(vertex_swarm_net_proto::pullsync::Want {
            bit_vector: self.wanted.into_bytes(),
        })
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        Ok(Self {
            wanted: BitVector::from_wire_bytes(proto.bit_vector),
        })
    }
}

/// A single wanted chunk: a full [`StampedChunk`], validated against its own
/// address on decode. Boxed to keep the enclosing message types small.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    pub chunk: Box<StampedChunk>,
}

impl Delivery {
    pub fn new(chunk: StampedChunk) -> Self {
        Self {
            chunk: Box::new(chunk),
        }
    }
}

impl ProtoMessage for Delivery {
    type Proto = vertex_swarm_net_proto::pullsync::Delivery;
    type EncodeError = std::convert::Infallible;
    type DecodeError = PullsyncError;

    fn into_proto(self) -> Result<Self::Proto, Self::EncodeError> {
        let address = *self.chunk.address();
        let (chunk, stamp) = (*self.chunk).into_parts();
        Ok(vertex_swarm_net_proto::pullsync::Delivery {
            address: address.to_vec(),
            data: chunk.into_bytes().to_vec(),
            stamp: stamp.to_bytes().to_vec(),
        })
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        if proto.address.len() != 32 {
            return Err(PullsyncError::InvalidFieldLength {
                field: "address",
                len: proto.address.len(),
            });
        }
        let address = ChunkAddress::from_slice(&proto.address)?;
        let stamp = nectar_postage::Stamp::try_from_slice(&proto.stamp)?;
        let chunk = StampedChunk::reconstruct(address, Bytes::from(proto.data), stamp)
            .map_err(|e| PullsyncError::InvalidChunk(e.to_string()))?;
        Ok(Self::new(chunk))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::Signature;
    use nectar_postage::Stamp;
    use nectar_primitives::{AnyChunk, ContentChunk};
    use vertex_net_codec::assert_proto_roundtrip;

    fn test_stamp() -> Stamp {
        let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
        Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, sig)
    }

    fn test_stamped_chunk() -> StampedChunk {
        let chunk: AnyChunk = ContentChunk::new(&b"pullsync payload"[..])
            .expect("valid content chunk")
            .into();
        StampedChunk::new(chunk, test_stamp())
    }

    fn descriptor() -> ChunkDescriptor {
        ChunkDescriptor::new(
            ChunkAddress::new([0x11; 32]),
            B256::repeat_byte(0x22),
            B256::repeat_byte(0x33),
        )
    }

    #[test]
    fn syn_roundtrip() {
        assert_proto_roundtrip!(Syn);
    }

    #[test]
    fn ack_roundtrip() {
        assert_proto_roundtrip!(Ack {
            cursors: vec![0, 5, 0, 9, 100],
            epoch: 7,
        });
    }

    #[test]
    fn get_roundtrip() {
        assert_proto_roundtrip!(Get::new(Bin::new(11).expect("valid bin"), 42));
    }

    #[test]
    fn get_rejects_out_of_range_bin() {
        let proto = vertex_swarm_net_proto::pullsync::Get { bin: 99, start: 0 };
        let err = Get::from_proto(proto).expect_err("bin 99 exceeds MAX_PO");
        assert!(matches!(err, PullsyncError::InvalidBin(99)));
    }

    #[test]
    fn get_rejects_negative_bin() {
        let proto = vertex_swarm_net_proto::pullsync::Get { bin: -1, start: 0 };
        let err = Get::from_proto(proto).expect_err("a negative bin is invalid");
        assert!(matches!(err, PullsyncError::InvalidBin(-1)));
    }

    #[test]
    fn offer_roundtrip() {
        assert_proto_roundtrip!(Offer::new(250, vec![descriptor(), descriptor()]));
    }

    #[test]
    fn offer_rejects_short_descriptor_field() {
        let proto = vertex_swarm_net_proto::pullsync::Offer {
            topmost: 1,
            chunks: vec![vertex_swarm_net_proto::pullsync::Chunk {
                address: vec![0u8; 32],
                batch_id: vec![0u8; 31],
                stamp_hash: vec![0u8; 32],
            }],
        };
        let err = Offer::from_proto(proto).expect_err("short batch_id rejected");
        assert!(matches!(
            err,
            PullsyncError::InvalidFieldLength {
                field: "batch_id",
                len: 31
            }
        ));
    }

    #[test]
    fn want_roundtrip_preserves_selection() {
        // The offer length is not on the wire, so a decoded `Want` is sized to a
        // whole number of bytes; the selected bits and the packed bytes are what
        // round-trip, not the original `len`.
        let mut bv = BitVector::new(10);
        bv.set(1);
        bv.set(9);
        let want = Want::new(bv);
        let decoded = Want::from_proto(want.clone().into_proto().unwrap()).unwrap();
        assert_eq!(decoded.wanted.as_bytes(), want.wanted.as_bytes());
        assert!(decoded.wanted.get(1));
        assert!(decoded.wanted.get(9));
        assert_eq!(decoded.count(), 2);
    }

    #[test]
    fn want_count_matches_set_bits() {
        let mut bv = BitVector::new(16);
        bv.set(0);
        bv.set(8);
        bv.set(15);
        assert_eq!(Want::new(bv).count(), 3);
    }

    #[test]
    fn delivery_roundtrip() {
        assert_proto_roundtrip!(Delivery::new(test_stamped_chunk()));
    }

    #[test]
    fn delivery_wire_data_is_chunk_identity() {
        let stamped = test_stamped_chunk();
        let address = *stamped.address();
        let wire_data = stamped.chunk().clone().into_bytes();
        let proto = Delivery::new(stamped).into_proto().unwrap();
        assert_eq!(Bytes::from(proto.data.clone()), wire_data);
        let decoded = Delivery::from_proto(proto).unwrap();
        assert_eq!(*decoded.chunk.address(), address);
    }
}
