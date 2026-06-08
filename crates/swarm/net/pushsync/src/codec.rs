//! Codec for pushsync protocol messages.

use alloy_primitives::Signature;
use bytes::Bytes;
use nectar_primitives::{ChunkAddress, Nonce};
use vertex_net_codec::{Codec, ProtoMessage};
use vertex_swarm_primitives::{Bin, Stamp, StorageRadius};

use crate::error::PushsyncError;

/// Codec for pushsync delivery messages.
pub(crate) type DeliveryCodec = Codec<Delivery, PushsyncError>;

/// Codec for pushsync receipt messages.
pub(crate) type ReceiptCodec = Codec<Receipt, PushsyncError>;

/// Delivery of a chunk to be stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    /// The address of the chunk.
    pub address: ChunkAddress,
    /// The chunk data.
    pub data: Bytes,
    /// The postage stamp attached to the chunk.
    pub stamp: Stamp,
}

impl Delivery {
    /// Create a new delivery.
    pub fn new(address: ChunkAddress, data: Bytes, stamp: Stamp) -> Self {
        Self {
            address,
            data,
            stamp,
        }
    }
}

impl ProtoMessage for Delivery {
    type Proto = vertex_swarm_net_proto::pushsync::Delivery;
    type EncodeError = std::convert::Infallible;
    type DecodeError = PushsyncError;

    fn into_proto(self) -> Result<Self::Proto, Self::EncodeError> {
        Ok(vertex_swarm_net_proto::pushsync::Delivery {
            address: self.address.to_vec(),
            data: self.data.to_vec(),
            stamp: self.stamp.to_bytes().to_vec(),
        })
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        if proto.address.len() != 32 {
            return Err(PushsyncError::InvalidAddressLength(proto.address.len()));
        }
        let address = ChunkAddress::from_slice(&proto.address)?;
        let stamp = Stamp::try_from_slice(&proto.stamp)?;
        Ok(Self {
            address,
            data: Bytes::from(proto.data),
            stamp,
        })
    }
}

/// Receipt acknowledging chunk storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    /// The address of the chunk.
    pub address: ChunkAddress,
    /// Signature from the storer.
    pub signature: Signature,
    /// Nonce used in signing.
    pub nonce: Nonce,
    /// Error message if storage failed.
    pub error: Option<String>,
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
            error: None,
            storage_radius,
        }
    }

    /// Create an error receipt.
    ///
    /// The signature, nonce, and radius are zero-valued placeholders; the
    /// `error` field is what consumers inspect.
    pub fn error(address: ChunkAddress, msg: impl Into<String>) -> Self {
        Self {
            address,
            signature: Signature::new(
                alloy_primitives::U256::ZERO,
                alloy_primitives::U256::ZERO,
                false,
            ),
            nonce: Nonce::ZERO,
            error: Some(msg.into()),
            storage_radius: StorageRadius::ZERO,
        }
    }

    /// Check if this receipt is an error.
    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }
}

impl ProtoMessage for Receipt {
    type Proto = vertex_swarm_net_proto::pushsync::Receipt;
    type EncodeError = std::convert::Infallible;
    type DecodeError = PushsyncError;

    fn into_proto(self) -> Result<Self::Proto, Self::EncodeError> {
        Ok(vertex_swarm_net_proto::pushsync::Receipt {
            address: self.address.to_vec(),
            signature: self.signature.as_bytes().to_vec(),
            nonce: self.nonce.as_slice().to_vec(),
            err: self.error.unwrap_or_default(),
            storage_radius: u32::from(self.storage_radius.get()),
        })
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        if proto.address.len() != 32 {
            return Err(PushsyncError::InvalidAddressLength(proto.address.len()));
        }
        let address = ChunkAddress::from_slice(&proto.address)?;
        // An error receipt carries the `err` string and may leave the
        // signature, nonce, and radius fields empty or zeroed. Do not parse
        // those fields in that case: a remote error receipt is decodable from
        // its `err` alone, mirroring the placeholders that [`Receipt::error`]
        // emits. Only a success receipt's typed fields are parsed strictly.
        if !proto.err.is_empty() {
            return Ok(Self::error(address, proto.err));
        }
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
        Ok(Self::success(address, signature, nonce, storage_radius))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use vertex_net_codec::assert_proto_roundtrip;

    /// A stamp with a deterministic, well-formed signature for roundtrip tests.
    fn test_stamp() -> Stamp {
        let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
        Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, sig)
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
        assert_proto_roundtrip!(Delivery::new(
            ChunkAddress::new([0x42; 32]),
            Bytes::from(vec![1, 2, 3, 4]),
            test_stamp(),
        ));
    }

    #[test]
    fn test_receipt_success_roundtrip() {
        let original = Receipt::success(
            ChunkAddress::new([0x42; 32]),
            test_signature(),
            Nonce::new([9u8; 32]),
            radius(10),
        );
        let proto = original.clone().into_proto().unwrap();
        let decoded = Receipt::from_proto(proto).unwrap();
        assert_eq!(original, decoded);
        assert!(!decoded.is_error());
    }

    #[test]
    fn test_receipt_error_roundtrip() {
        let original = Receipt::error(ChunkAddress::new([0x42; 32]), "storage failed");
        let proto = original.clone().into_proto().unwrap();
        let decoded = Receipt::from_proto(proto).unwrap();
        assert_eq!(original, decoded);
        assert!(decoded.is_error());
    }
}
