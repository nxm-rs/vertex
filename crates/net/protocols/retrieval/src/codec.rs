//! Codec for retrieval protocol messages.

use bytes::Bytes;
use vertex_net_codec::ProtocolCodec;
use vertex_primitives::ChunkAddress;

/// Codec for retrieval request messages.
pub type RequestCodec = ProtocolCodec<crate::proto::retrieval::Request, Request, RetrievalCodecError>;

/// Codec for retrieval delivery messages.
pub type DeliveryCodec =
    ProtocolCodec<crate::proto::retrieval::Delivery, Delivery, RetrievalCodecError>;

/// Error type for retrieval codec operations.
#[derive(Debug, thiserror::Error)]
pub enum RetrievalCodecError {
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

impl From<quick_protobuf_codec::Error> for RetrievalCodecError {
    fn from(error: quick_protobuf_codec::Error) -> Self {
        RetrievalCodecError::Protocol(error.to_string())
    }
}

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

impl TryFrom<crate::proto::retrieval::Request> for Request {
    type Error = RetrievalCodecError;

    fn try_from(value: crate::proto::retrieval::Request) -> Result<Self, Self::Error> {
        if value.Addr.len() != 32 {
            return Err(RetrievalCodecError::InvalidAddressLength(value.Addr.len()));
        }
        let address = ChunkAddress::from_slice(&value.Addr)
            .map_err(|e| RetrievalCodecError::Protocol(e.to_string()))?;
        Ok(Self { address })
    }
}

impl From<Request> for crate::proto::retrieval::Request {
    fn from(value: Request) -> Self {
        crate::proto::retrieval::Request {
            Addr: value.address.to_vec(),
        }
    }
}

/// Delivery of a chunk with optional error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    /// The chunk data (empty if error).
    pub data: Bytes,
    /// The postage stamp attached to the chunk.
    pub stamp: Bytes,
    /// Error message if retrieval failed.
    pub error: Option<String>,
}

impl Delivery {
    /// Create a successful delivery.
    pub fn success(data: Bytes, stamp: Bytes) -> Self {
        Self {
            data,
            stamp,
            error: None,
        }
    }

    /// Create an error delivery.
    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            data: Bytes::new(),
            stamp: Bytes::new(),
            error: Some(msg.into()),
        }
    }

    /// Check if this delivery is an error.
    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }
}

impl TryFrom<crate::proto::retrieval::Delivery> for Delivery {
    type Error = RetrievalCodecError;

    fn try_from(value: crate::proto::retrieval::Delivery) -> Result<Self, Self::Error> {
        let error = if value.Err_pb.is_empty() {
            None
        } else {
            Some(value.Err_pb)
        };
        Ok(Self {
            data: Bytes::from(value.Data),
            stamp: Bytes::from(value.Stamp),
            error,
        })
    }
}

impl From<Delivery> for crate::proto::retrieval::Delivery {
    fn from(value: Delivery) -> Self {
        crate::proto::retrieval::Delivery {
            Data: value.data.to_vec(),
            Stamp: value.stamp.to_vec(),
            Err_pb: value.error.unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_roundtrip() {
        let original = Request::new(ChunkAddress::new([0x42; 32]));
        let proto: crate::proto::retrieval::Request = original.clone().into();
        let decoded = Request::try_from(proto).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_delivery_success_roundtrip() {
        let original = Delivery::success(Bytes::from(vec![1, 2, 3, 4]), Bytes::from(vec![5, 6, 7]));
        let proto: crate::proto::retrieval::Delivery = original.clone().into();
        let decoded = Delivery::try_from(proto).unwrap();
        assert_eq!(original, decoded);
        assert!(!decoded.is_error());
    }

    #[test]
    fn test_delivery_error_roundtrip() {
        let original = Delivery::error("chunk not found");
        let proto: crate::proto::retrieval::Delivery = original.clone().into();
        let decoded = Delivery::try_from(proto).unwrap();
        assert_eq!(original, decoded);
        assert!(decoded.is_error());
    }
}
