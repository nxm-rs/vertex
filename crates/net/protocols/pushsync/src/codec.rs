//! Codec for pushsync protocol messages.

use bytes::Bytes;
use vertex_net_codec::{Codec, ProtoMessage, ProtocolCodecError};
use vertex_primitives::ChunkAddress;

/// Domain-specific errors for pushsync protocol.
#[derive(Debug, thiserror::Error)]
pub enum PushsyncError {
    /// Invalid chunk address length
    #[error("Invalid chunk address length: expected 32, got {0}")]
    InvalidAddressLength(usize),
}

/// Error type for pushsync codec operations.
pub type PushsyncCodecError = ProtocolCodecError<PushsyncError>;

/// Codec for pushsync delivery messages.
pub type DeliveryCodec = Codec<Delivery, PushsyncCodecError>;

/// Codec for pushsync receipt messages.
pub type ReceiptCodec = Codec<Receipt, PushsyncCodecError>;

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
    type Proto = crate::proto::pushsync::Delivery;
    type DecodeError = PushsyncCodecError;

    fn into_proto(self) -> Self::Proto {
        crate::proto::pushsync::Delivery {
            Address: self.address.to_vec(),
            Data: self.data.to_vec(),
            Stamp: self.stamp.to_vec(),
        }
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        if proto.Address.len() != 32 {
            return Err(PushsyncCodecError::domain(
                PushsyncError::InvalidAddressLength(proto.Address.len()),
            ));
        }
        let address = ChunkAddress::from_slice(&proto.Address)
            .map_err(|e| PushsyncCodecError::protocol(e.to_string()))?;
        Ok(Self {
            address,
            data: Bytes::from(proto.Data),
            stamp: Bytes::from(proto.Stamp),
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
    type Proto = crate::proto::pushsync::Receipt;
    type DecodeError = PushsyncCodecError;

    fn into_proto(self) -> Self::Proto {
        crate::proto::pushsync::Receipt {
            Address: self.address.to_vec(),
            Signature: self.signature.to_vec(),
            Nonce: self.nonce.to_vec(),
            Err_pb: self.error.unwrap_or_default(),
            StorageRadius: self.storage_radius as u32,
        }
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        if proto.Address.len() != 32 {
            return Err(PushsyncCodecError::domain(
                PushsyncError::InvalidAddressLength(proto.Address.len()),
            ));
        }
        let address = ChunkAddress::from_slice(&proto.Address)
            .map_err(|e| PushsyncCodecError::protocol(e.to_string()))?;
        let error = if proto.Err_pb.is_empty() {
            None
        } else {
            Some(proto.Err_pb)
        };
        Ok(Self {
            address,
            signature: Bytes::from(proto.Signature),
            nonce: Bytes::from(proto.Nonce),
            error,
            storage_radius: proto.StorageRadius as u8,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_delivery_roundtrip() {
        let original = Delivery::new(
            ChunkAddress::new([0x42; 32]),
            Bytes::from(vec![1, 2, 3, 4]),
            Bytes::from(vec![5, 6, 7]),
        );
        let proto = original.clone().into_proto();
        let decoded = Delivery::from_proto(proto).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_receipt_success_roundtrip() {
        let original = Receipt::success(
            ChunkAddress::new([0x42; 32]),
            Bytes::from(vec![1, 2, 3, 4]),
            Bytes::from(vec![5, 6, 7]),
            10,
        );
        let proto = original.clone().into_proto();
        let decoded = Receipt::from_proto(proto).unwrap();
        assert_eq!(original, decoded);
        assert!(!decoded.is_error());
    }

    #[test]
    fn test_receipt_error_roundtrip() {
        let original = Receipt::error(ChunkAddress::new([0x42; 32]), "storage failed");
        let proto = original.clone().into_proto();
        let decoded = Receipt::from_proto(proto).unwrap();
        assert_eq!(original, decoded);
        assert!(decoded.is_error());
    }
}
