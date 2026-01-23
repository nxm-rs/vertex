//! Codec for pushsync protocol messages.

use bytes::Bytes;
use vertex_net_codec::ProtocolCodec;
use vertex_primitives::ChunkAddress;

/// Codec for pushsync delivery messages.
pub type DeliveryCodec =
    ProtocolCodec<crate::proto::pushsync::Delivery, Delivery, PushsyncCodecError>;

/// Codec for pushsync receipt messages.
pub type ReceiptCodec = ProtocolCodec<crate::proto::pushsync::Receipt, Receipt, PushsyncCodecError>;

/// Error type for pushsync codec operations.
#[derive(Debug, thiserror::Error)]
pub enum PushsyncCodecError {
    /// Protocol-level error (invalid message format, etc.)
    #[error("Protocol error: {0}")]
    Protocol(String),
    /// IO error during read/write
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    /// Invalid chunk address length
    #[error("Invalid chunk address length: expected 32, got {0}")]
    InvalidAddressLength(usize),
}

impl From<quick_protobuf_codec::Error> for PushsyncCodecError {
    fn from(error: quick_protobuf_codec::Error) -> Self {
        PushsyncCodecError::Protocol(error.to_string())
    }
}

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

impl TryFrom<crate::proto::pushsync::Delivery> for Delivery {
    type Error = PushsyncCodecError;

    fn try_from(value: crate::proto::pushsync::Delivery) -> Result<Self, Self::Error> {
        if value.Address.len() != 32 {
            return Err(PushsyncCodecError::InvalidAddressLength(
                value.Address.len(),
            ));
        }
        let address = ChunkAddress::from_slice(&value.Address)
            .map_err(|e| PushsyncCodecError::Protocol(e.to_string()))?;
        Ok(Self {
            address,
            data: Bytes::from(value.Data),
            stamp: Bytes::from(value.Stamp),
        })
    }
}

impl From<Delivery> for crate::proto::pushsync::Delivery {
    fn from(value: Delivery) -> Self {
        crate::proto::pushsync::Delivery {
            Address: value.address.to_vec(),
            Data: value.data.to_vec(),
            Stamp: value.stamp.to_vec(),
        }
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

impl TryFrom<crate::proto::pushsync::Receipt> for Receipt {
    type Error = PushsyncCodecError;

    fn try_from(value: crate::proto::pushsync::Receipt) -> Result<Self, Self::Error> {
        if value.Address.len() != 32 {
            return Err(PushsyncCodecError::InvalidAddressLength(
                value.Address.len(),
            ));
        }
        let address = ChunkAddress::from_slice(&value.Address)
            .map_err(|e| PushsyncCodecError::Protocol(e.to_string()))?;
        let error = if value.Err_pb.is_empty() {
            None
        } else {
            Some(value.Err_pb)
        };
        Ok(Self {
            address,
            signature: Bytes::from(value.Signature),
            nonce: Bytes::from(value.Nonce),
            error,
            storage_radius: value.StorageRadius as u8,
        })
    }
}

impl From<Receipt> for crate::proto::pushsync::Receipt {
    fn from(value: Receipt) -> Self {
        crate::proto::pushsync::Receipt {
            Address: value.address.to_vec(),
            Signature: value.signature.to_vec(),
            Nonce: value.nonce.to_vec(),
            Err_pb: value.error.unwrap_or_default(),
            StorageRadius: value.storage_radius as u32,
        }
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
        let proto: crate::proto::pushsync::Delivery = original.clone().into();
        let decoded = Delivery::try_from(proto).unwrap();
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
        let proto: crate::proto::pushsync::Receipt = original.clone().into();
        let decoded = Receipt::try_from(proto).unwrap();
        assert_eq!(original, decoded);
        assert!(!decoded.is_error());
    }

    #[test]
    fn test_receipt_error_roundtrip() {
        let original = Receipt::error(ChunkAddress::new([0x42; 32]), "storage failed");
        let proto: crate::proto::pushsync::Receipt = original.clone().into();
        let decoded = Receipt::try_from(proto).unwrap();
        assert_eq!(original, decoded);
        assert!(decoded.is_error());
    }
}
