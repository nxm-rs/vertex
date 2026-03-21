//! Codec for credit protocol messages.
//!
//! # Wire Format
//!
//! The credit limit is encoded as big-endian bytes with leading zeros trimmed,
//! matching Go's `big.Int.Bytes()` serialisation used by Bee.

use alloy_primitives::U256;
use vertex_net_codec::{Codec, ProtoMessage, decode_u256_be, encode_u256_be};

use crate::error::CreditError;

/// Codec for credit protocol messages.
pub(crate) type CreditCodec = Codec<AnnounceCreditLimit, CreditError>;

/// Credit limit announcement message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnounceCreditLimit {
    /// The credit limit in accounting units.
    pub credit_limit: U256,
}

impl AnnounceCreditLimit {
    /// Create a new announcement with the given credit limit.
    pub fn new(credit_limit: U256) -> Self {
        Self { credit_limit }
    }

    /// Create from a u64 credit limit value.
    pub fn from_u64(limit: u64) -> Self {
        Self::new(U256::from(limit))
    }
}

impl ProtoMessage for AnnounceCreditLimit {
    type Proto = vertex_swarm_net_proto::pricing::AnnounceCreditLimit;
    type EncodeError = std::convert::Infallible;
    type DecodeError = CreditError;

    fn into_proto(self) -> Result<Self::Proto, Self::EncodeError> {
        Ok(vertex_swarm_net_proto::pricing::AnnounceCreditLimit {
            credit_limit: encode_u256_be(self.credit_limit),
        })
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        Ok(Self {
            credit_limit: decode_u256_be(&proto.credit_limit),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_net_codec::assert_proto_roundtrip;

    #[test]
    fn test_roundtrip() {
        assert_proto_roundtrip!(AnnounceCreditLimit::from_u64(13_500_000));
    }

    #[test]
    fn test_zero_limit() {
        let original = AnnounceCreditLimit::new(U256::ZERO);
        let proto = original.clone().into_proto().unwrap();
        assert!(proto.credit_limit.is_empty());
        let decoded = AnnounceCreditLimit::from_proto(proto).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_large_limit() {
        let original = AnnounceCreditLimit::new(U256::MAX);
        let proto = original.clone().into_proto().unwrap();
        assert_eq!(proto.credit_limit.len(), 32);
        let decoded = AnnounceCreditLimit::from_proto(proto).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_small_value_minimal_bytes() {
        let original = AnnounceCreditLimit::from_u64(256);
        let proto = original.clone().into_proto().unwrap();
        assert_eq!(proto.credit_limit, vec![0x01, 0x00]);
    }
}
