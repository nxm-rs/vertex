//! Codec for pricing protocol messages.

use alloy_primitives::U256;
use vertex_net_codec::ProtocolCodec;

/// Codec for pricing protocol messages.
pub(crate) type PricingCodec =
    ProtocolCodec<crate::proto::pricing::AnnouncePaymentThreshold, AnnouncePaymentThreshold, PricingCodecError>;

/// Error type for pricing codec operations.
#[derive(Debug, thiserror::Error)]
pub enum PricingCodecError {
    /// Protocol-level error (invalid message format, etc.)
    #[error("Protocol error: {0}")]
    Protocol(String),
    /// IO error during read/write
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<quick_protobuf_codec::Error> for PricingCodecError {
    fn from(error: quick_protobuf_codec::Error) -> Self {
        PricingCodecError::Protocol(error.to_string())
    }
}

/// Payment threshold announcement message.
///
/// Contains the payment threshold as a U256 value.
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

impl TryFrom<crate::proto::pricing::AnnouncePaymentThreshold> for AnnouncePaymentThreshold {
    type Error = PricingCodecError;

    fn try_from(value: crate::proto::pricing::AnnouncePaymentThreshold) -> Result<Self, Self::Error> {
        // Payment threshold is encoded as big-endian bytes
        let threshold = if value.payment_threshold.is_empty() {
            U256::ZERO
        } else {
            U256::from_be_slice(&value.payment_threshold)
        };

        Ok(Self {
            payment_threshold: threshold,
        })
    }
}

impl From<AnnouncePaymentThreshold> for crate::proto::pricing::AnnouncePaymentThreshold {
    fn from(value: AnnouncePaymentThreshold) -> Self {
        // Encode threshold as big-endian bytes, trimming leading zeros
        let bytes = value.payment_threshold.to_be_bytes_vec();

        // Find first non-zero byte to trim leading zeros (like Go's big.Int.Bytes())
        let first_nonzero = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
        let trimmed = bytes[first_nonzero..].to_vec();

        crate::proto::pricing::AnnouncePaymentThreshold {
            payment_threshold: trimmed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip() {
        let original = AnnouncePaymentThreshold::from_u64(13_500_000);
        let proto: crate::proto::pricing::AnnouncePaymentThreshold = original.clone().into();
        let decoded = AnnouncePaymentThreshold::try_from(proto).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_zero_threshold() {
        let original = AnnouncePaymentThreshold::new(U256::ZERO);
        let proto: crate::proto::pricing::AnnouncePaymentThreshold = original.clone().into();
        let decoded = AnnouncePaymentThreshold::try_from(proto).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_large_threshold() {
        let original = AnnouncePaymentThreshold::new(U256::MAX);
        let proto: crate::proto::pricing::AnnouncePaymentThreshold = original.clone().into();
        let decoded = AnnouncePaymentThreshold::try_from(proto).unwrap();
        assert_eq!(original, decoded);
    }
}
