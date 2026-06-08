//! Codec for retrieval protocol messages.

use bytes::Bytes;
use nectar_primitives::ChunkAddress;
use vertex_net_codec::{Codec, ProtoMessage};
use vertex_swarm_primitives::Stamp;

use crate::error::RetrievalError;

/// Codec for retrieval request messages.
pub(crate) type RequestCodec = Codec<Request, RetrievalError>;

/// Codec for retrieval delivery messages.
pub(crate) type DeliveryCodec = Codec<Delivery, RetrievalError>;

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
        let address = ChunkAddress::from_slice(&proto.addr)
            .map_err(|e| RetrievalError::InvalidAddress(e.to_string()))?;
        Ok(Self { address })
    }
}

/// Delivery of a chunk with optional error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    /// The chunk data (empty if error).
    pub data: Bytes,
    /// The postage stamp attached to the chunk. Absent on an error delivery.
    pub stamp: Option<Stamp>,
    /// Error message if retrieval failed.
    pub error: Option<String>,
}

impl Delivery {
    /// Create a successful delivery.
    pub fn success(data: Bytes, stamp: Stamp) -> Self {
        Self {
            data,
            stamp: Some(stamp),
            error: None,
        }
    }

    /// Create an error delivery.
    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            data: Bytes::new(),
            stamp: None,
            error: Some(msg.into()),
        }
    }

    /// Check if this delivery is an error.
    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }
}

impl ProtoMessage for Delivery {
    type Proto = vertex_swarm_net_proto::retrieval::Delivery;
    type EncodeError = std::convert::Infallible;
    type DecodeError = RetrievalError;

    fn into_proto(self) -> Result<Self::Proto, Self::EncodeError> {
        Ok(vertex_swarm_net_proto::retrieval::Delivery {
            data: self.data.to_vec(),
            stamp: self
                .stamp
                .map(|s| s.to_bytes().to_vec())
                .unwrap_or_default(),
            err: self.error.unwrap_or_default(),
        })
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        // An error delivery carries the `err` string and no stamp. Do not parse
        // the stamp field in that case: the delivery is decodable from its
        // `err` alone. Only a success delivery's stamp is parsed strictly.
        if !proto.err.is_empty() {
            return Ok(Self::error(proto.err));
        }
        let stamp = Stamp::try_from_slice(&proto.stamp)
            .map_err(|e| RetrievalError::InvalidStamp(e.to_string()))?;
        Ok(Self {
            data: Bytes::from(proto.data),
            stamp: Some(stamp),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{B256, Signature};
    use vertex_net_codec::assert_proto_roundtrip;

    /// A stamp with a deterministic, well-formed signature for roundtrip tests.
    fn test_stamp() -> Stamp {
        let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
        Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, sig)
    }

    #[test]
    fn test_request_roundtrip() {
        assert_proto_roundtrip!(Request::new(ChunkAddress::new([0x42; 32])));
    }

    #[test]
    fn test_delivery_success_roundtrip() {
        let original = Delivery::success(Bytes::from(vec![1, 2, 3, 4]), test_stamp());
        let proto = original.clone().into_proto().unwrap();
        let decoded = Delivery::from_proto(proto).unwrap();
        assert_eq!(original, decoded);
        assert!(!decoded.is_error());
    }

    #[test]
    fn test_delivery_error_roundtrip() {
        let original = Delivery::error("chunk not found");
        let proto = original.clone().into_proto().unwrap();
        let decoded = Delivery::from_proto(proto).unwrap();
        assert_eq!(original, decoded);
        assert!(decoded.is_error());
    }
}
