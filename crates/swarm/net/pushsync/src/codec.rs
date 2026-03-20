//! Codec for pushsync protocol messages.

use bytes::Bytes;
use nectar_primitives::ChunkAddress;
use vertex_net_codec::{Codec, ProtoMessage};

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
    pub stamp: Bytes,
}

impl Delivery {
    /// Create a new delivery.
    pub fn new(address: ChunkAddress, data: Bytes, stamp: Bytes) -> Self {
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
            stamp: self.stamp.to_vec(),
        })
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        if proto.address.len() != 32 {
            return Err(PushsyncError::InvalidAddressLength(proto.address.len()));
        }
        let address = ChunkAddress::from_slice(&proto.address)
            .map_err(|e| PushsyncError::InvalidAddress(e.to_string()))?;
        Ok(Self {
            address,
            data: Bytes::from(proto.data),
            stamp: Bytes::from(proto.stamp),
        })
    }
}

/// Receipt acknowledging chunk storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    /// The address of the chunk.
    pub address: ChunkAddress,
    /// Signature from the storer.
    pub signature: Bytes,
    /// Nonce used in signing.
    pub nonce: Bytes,
    /// Error message if storage failed.
    pub error: Option<String>,
    /// The storage radius of the storer node.
    pub storage_radius: u8,
}

impl Receipt {
    /// Create a successful receipt.
    pub fn success(
        address: ChunkAddress,
        signature: Bytes,
        nonce: Bytes,
        storage_radius: u8,
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
    pub fn error(address: ChunkAddress, msg: impl Into<String>) -> Self {
        Self {
            address,
            signature: Bytes::new(),
            nonce: Bytes::new(),
            error: Some(msg.into()),
            storage_radius: 0,
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
            signature: self.signature.to_vec(),
            nonce: self.nonce.to_vec(),
            err: self.error.unwrap_or_default(),
            storage_radius: self.storage_radius as u32,
        })
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        if proto.address.len() != 32 {
            return Err(PushsyncError::InvalidAddressLength(proto.address.len()));
        }
        let address = ChunkAddress::from_slice(&proto.address)
            .map_err(|e| PushsyncError::InvalidAddress(e.to_string()))?;
        let error = if proto.err.is_empty() {
            None
        } else {
            Some(proto.err)
        };
        let storage_radius = u8::try_from(proto.storage_radius)
            .map_err(|_| PushsyncError::InvalidStorageRadius(proto.storage_radius))?;
        Ok(Self {
            address,
            signature: Bytes::from(proto.signature),
            nonce: Bytes::from(proto.nonce),
            error,
            storage_radius,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_net_codec::assert_proto_roundtrip;

    #[test]
    fn test_delivery_roundtrip() {
        assert_proto_roundtrip!(Delivery::new(
            ChunkAddress::new([0x42; 32]),
            Bytes::from(vec![1, 2, 3, 4]),
            Bytes::from(vec![5, 6, 7]),
        ));
    }

    #[test]
    fn test_receipt_success_roundtrip() {
        let original = Receipt::success(
            ChunkAddress::new([0x42; 32]),
            Bytes::from(vec![1, 2, 3, 4]),
            Bytes::from(vec![5, 6, 7]),
            10,
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
