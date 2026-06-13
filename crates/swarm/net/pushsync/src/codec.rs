//! Codec for pushsync protocol messages.

use alloy_primitives::Signature;
use bytes::Bytes;
use nectar_primitives::{ChunkAddress, Nonce};
use vertex_net_codec::{Codec, ProtoMessage};
use vertex_swarm_primitives::{Bin, StampedChunk, StorageRadius};

use crate::error::PushsyncError;

/// Codec for pushsync delivery messages.
pub(crate) type DeliveryCodec = Codec<Delivery, PushsyncError>;

/// Codec for pushsync receipt responses.
pub(crate) type ReceiptCodec = Codec<ReceiptResponse, PushsyncError>;

/// Delivery of a chunk to be stored.
///
/// Carries the chunk and its postage stamp as one [`StampedChunk`]. The wire
/// `address` field is the chunk's own address; on decode it disambiguates and
/// validates the reconstructed chunk.
///
/// The pairing is boxed: a [`StampedChunk`] is large, and boxing it here keeps
/// the message enums that carry a delivery small.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    /// The chunk paired with its postage stamp.
    pub chunk: Box<StampedChunk>,
}

impl Delivery {
    /// Create a new delivery.
    pub fn new(chunk: StampedChunk) -> Self {
        Self {
            chunk: Box::new(chunk),
        }
    }
}

impl ProtoMessage for Delivery {
    type Proto = vertex_swarm_net_proto::pushsync::Delivery;
    type EncodeError = std::convert::Infallible;
    type DecodeError = PushsyncError;

    fn into_proto(self) -> Result<Self::Proto, Self::EncodeError> {
        let address = *self.chunk.address();
        let (chunk, stamp) = (*self.chunk).into_parts();
        Ok(vertex_swarm_net_proto::pushsync::Delivery {
            address: address.to_vec(),
            data: chunk.into_bytes().to_vec(),
            stamp: stamp.to_bytes().to_vec(),
        })
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        if proto.address.len() != 32 {
            return Err(PushsyncError::InvalidAddressLength(proto.address.len()));
        }
        let address = ChunkAddress::from_slice(&proto.address)?;
        let stamp = nectar_postage::Stamp::try_from_slice(&proto.stamp)?;
        let chunk = StampedChunk::reconstruct(address, Bytes::from(proto.data), stamp)?;
        Ok(Self::new(chunk))
    }
}

/// Length in bytes of a well-formed recoverable signature. A failure response
/// carries an empty signature instead, which is the structural failure signal.
const SIGNATURE_LEN: usize = 65;

/// A successful storage receipt.
///
/// This type models a custody acknowledgement only. The reference does not sign
/// its failure responses (a failure is `&pb.Receipt{Err: ...}` with no
/// signature), so there is no signed failure receipt to model and no need for
/// placeholder fields. A failure is represented by the [`ReceiptResponse::Failed`]
/// variant, not by a `Receipt` with zeroed fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    /// The address of the chunk.
    pub address: ChunkAddress,
    /// Signature from the storer over the chunk address and nonce.
    pub signature: Signature,
    /// Nonce used in signing.
    pub nonce: Nonce,
    /// The storage radius of the storer node.
    pub storage_radius: StorageRadius,
}

impl Receipt {
    /// Create a successful receipt.
    pub fn success(
        address: ChunkAddress,
        signature: Signature,
        nonce: Nonce,
        storage_radius: StorageRadius,
    ) -> Self {
        Self {
            address,
            signature,
            nonce,
            storage_radius,
        }
    }
}

/// The decoded response to a pushsync delivery: a signed receipt on success, or
/// a structural failure.
///
/// Modelling success-or-failure here keeps [`Receipt`] free of placeholder
/// fields. A failure is never put on the wire by this implementation: the
/// responder signals failure by resetting the stream (see
/// `PushsyncResponder::send_error`). The [`Failed`](Self::Failed) encode arm
/// emits an empty frame purely so the codec round-trips; nothing reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptResponse {
    /// The storer took custody and returned a signed receipt.
    Stored(Receipt),
    /// The storer could not take custody. The reference's failure response is
    /// unsigned; its reason string is adversarial input we never read.
    Failed,
}

impl ReceiptResponse {
    /// Check if this response reports a failure.
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Failed)
    }
}

impl ProtoMessage for ReceiptResponse {
    type Proto = vertex_swarm_net_proto::pushsync::Receipt;
    type EncodeError = std::convert::Infallible;
    type DecodeError = PushsyncError;

    fn into_proto(self) -> Result<Self::Proto, Self::EncodeError> {
        match self {
            // An empty signature is the structural failure signal. In practice a
            // responder signals failure by resetting the stream (see
            // `PushsyncResponder::send_error`), so this frame is never actually
            // encoded onto the wire; this arm exists only for codec round-trip
            // symmetry with the decode path and carries no meaningful payload.
            Self::Failed => Ok(vertex_swarm_net_proto::pushsync::Receipt::default()),
            Self::Stored(receipt) => Ok(vertex_swarm_net_proto::pushsync::Receipt {
                address: receipt.address.to_vec(),
                signature: receipt.signature.as_bytes().to_vec(),
                nonce: receipt.nonce.as_slice().to_vec(),
                storage_radius: u32::from(receipt.storage_radius.get()),
            }),
        }
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        // Failure is detected structurally: a failure response carries no full
        // signature. Any error marker on the opaque `err` field is ignored and
        // never read. Only a success receipt's typed fields are parsed strictly.
        if proto.signature.len() != SIGNATURE_LEN {
            return Ok(Self::Failed);
        }
        if proto.address.len() != 32 {
            return Err(PushsyncError::InvalidAddressLength(proto.address.len()));
        }
        let address = ChunkAddress::from_slice(&proto.address)?;
        let signature = Signature::from_raw(&proto.signature)?;
        let nonce_bytes: [u8; 32] = proto
            .nonce
            .as_slice()
            .try_into()
            .map_err(|_| PushsyncError::InvalidNonceLength(proto.nonce.len()))?;
        let nonce = Nonce::new(nonce_bytes);
        let radius_byte = u8::try_from(proto.storage_radius)
            .map_err(|_| PushsyncError::InvalidStorageRadius(proto.storage_radius))?;
        let storage_radius = StorageRadius::new(
            Bin::new(radius_byte)
                .map_err(|_| PushsyncError::InvalidStorageRadius(proto.storage_radius))?,
        );
        Ok(Self::Stored(Receipt::success(
            address,
            signature,
            nonce,
            storage_radius,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use nectar_postage::Stamp;
    use nectar_primitives::{AnyChunk, ContentChunk};
    use vertex_net_codec::assert_proto_roundtrip;

    /// A stamp with a deterministic, well-formed signature for roundtrip tests.
    fn test_stamp() -> Stamp {
        let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
        Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, sig)
    }

    fn test_stamped_chunk() -> StampedChunk {
        let chunk: AnyChunk = ContentChunk::new(&b"pushsync payload"[..])
            .expect("valid content chunk")
            .into();
        StampedChunk::new(chunk, test_stamp())
    }

    fn test_signature() -> Signature {
        let mut raw = [2u8; 65];
        raw[64] = 0; // valid recovery id (parity)
        Signature::from_raw(&raw).expect("valid signature")
    }

    fn radius(value: u8) -> StorageRadius {
        StorageRadius::new(Bin::new(value).expect("valid bin"))
    }

    #[test]
    fn test_delivery_roundtrip() {
        assert_proto_roundtrip!(Delivery::new(test_stamped_chunk()));
    }

    #[test]
    fn test_delivery_wire_bytes_are_chunk_identity() {
        // Encode = chunk.into_bytes(); decode reconstructs byte-identically.
        let stamped = test_stamped_chunk();
        let address = *stamped.address();
        let wire_data = stamped.chunk().clone().into_bytes();
        let proto = Delivery::new(stamped).into_proto().unwrap();
        assert_eq!(Bytes::from(proto.data.clone()), wire_data);
        let decoded = Delivery::from_proto(proto).unwrap();
        assert_eq!(*decoded.chunk.address(), address);
        assert_eq!(decoded.chunk.chunk().clone().into_bytes(), wire_data);
    }

    #[test]
    fn test_receipt_success_roundtrip() {
        let original = ReceiptResponse::Stored(Receipt::success(
            ChunkAddress::new([0x42; 32]),
            test_signature(),
            Nonce::new([9u8; 32]),
            radius(10),
        ));
        let proto = original.clone().into_proto().unwrap();
        let decoded = ReceiptResponse::from_proto(proto).unwrap();
        assert_eq!(original, decoded);
        assert!(!decoded.is_error());
    }

    #[test]
    fn test_receipt_error_roundtrip() {
        let original = ReceiptResponse::Failed;
        let proto = original.clone().into_proto().unwrap();
        // A failure is detected structurally by an empty signature.
        assert!(proto.signature.is_empty());
        let decoded = ReceiptResponse::from_proto(proto).unwrap();
        assert_eq!(original, decoded);
        assert!(decoded.is_error());
    }
}
