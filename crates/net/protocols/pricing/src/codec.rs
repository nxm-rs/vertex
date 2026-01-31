//! Codec for pricing protocol messages.
//!
//! # Wire Format
//!
//! The payment threshold is encoded as big-endian bytes with leading zeros trimmed,
//! matching Go's `big.Int.Bytes()` serialization used by Bee.

use alloy_primitives::U256;
use vertex_net_codec::{Codec, ProtoMessage, ProtocolCodecError};

/// Error type for pricing codec operations.
///
/// Pricing has no domain-specific errors.
pub type PricingCodecError = ProtocolCodecError;

/// Codec for pricing protocol messages.
pub(crate) type PricingCodec = Codec<AnnouncePaymentThreshold, PricingCodecError>;

/// Payment threshold announcement message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnouncePaymentThreshold {
    /// The payment threshold in accounting units.
    pub payment_threshold: U256,
}

impl AnnouncePaymentThreshold {
    /// Create a new announcement with the given threshold.
    pub fn new(payment_threshold: U256) -> Self {
        Self { payment_threshold }
    }

    /// Create from a u64 threshold value.
    pub fn from_u64(threshold: u64) -> Self {
        Self::new(U256::from(threshold))
    }
}

impl ProtoMessage for AnnouncePaymentThreshold {
    type Proto = crate::proto::pricing::AnnouncePaymentThreshold;
    type DecodeError = PricingCodecError;

    fn into_proto(self) -> Self::Proto {
        // Match Go's big.Int.Bytes(): big-endian with leading zeros trimmed
        let bytes = self.payment_threshold.to_be_bytes::<32>();
        let trimmed = match bytes.iter().position(|&b| b != 0) {
            Some(pos) => bytes[pos..].to_vec(),
            None => vec![],
        };

        crate::proto::pricing::AnnouncePaymentThreshold {
            payment_threshold: trimmed,
        }
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        let threshold = if proto.payment_threshold.is_empty() {
            U256::ZERO
        } else {
            U256::from_be_slice(&proto.payment_threshold)
        };

        Ok(Self {
            payment_threshold: threshold,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip() {
        let original = AnnouncePaymentThreshold::from_u64(13_500_000);
        let proto = original.clone().into_proto();
        let decoded = AnnouncePaymentThreshold::from_proto(proto).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_zero_threshold() {
        let original = AnnouncePaymentThreshold::new(U256::ZERO);
        let proto = original.clone().into_proto();
        assert!(proto.payment_threshold.is_empty());
        let decoded = AnnouncePaymentThreshold::from_proto(proto).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_large_threshold() {
        let original = AnnouncePaymentThreshold::new(U256::MAX);
        let proto = original.clone().into_proto();
        assert_eq!(proto.payment_threshold.len(), 32);
        let decoded = AnnouncePaymentThreshold::from_proto(proto).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_small_value_minimal_bytes() {
        let original = AnnouncePaymentThreshold::from_u64(256);
        let proto = original.clone().into_proto();
        assert_eq!(proto.payment_threshold, vec![0x01, 0x00]);
    }
}
