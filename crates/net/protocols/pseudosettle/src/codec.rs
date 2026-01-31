//! Codec for pseudosettle protocol messages.
//!
//! Provides separate typed codecs for request and response:
//! - `PaymentCodec` - Encodes/decodes `Payment` messages only
//! - `PaymentAckCodec` - Encodes/decodes `PaymentAck` messages only
//!
//! Amounts are encoded as big-endian bytes with leading zeros trimmed,
//! matching Go's `big.Int.Bytes()` serialization used by Bee.

use alloy_primitives::U256;
use vertex_net_codec::{Codec, ProtoMessage, ProtocolCodecError};

/// Domain-specific errors for pseudosettle protocol.
#[derive(Debug, thiserror::Error)]
pub enum PseudosettleError {
    #[error("Invalid timestamp: {0}")]
    InvalidTimestamp(String),
}

/// Error type for pseudosettle codec operations.
pub type PseudosettleCodecError = ProtocolCodecError<PseudosettleError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payment {
    pub amount: U256,
}

impl Payment {
    pub fn new(amount: U256) -> Self {
        Self { amount }
    }

    pub fn from_u64(amount: u64) -> Self {
        Self::new(U256::from(amount))
    }
}

impl ProtoMessage for Payment {
    type Proto = crate::proto::pseudosettle::Payment;
    type DecodeError = PseudosettleCodecError;

    fn into_proto(self) -> Self::Proto {
        let bytes = self.amount.to_be_bytes::<32>();
        let trimmed = match bytes.iter().position(|&b| b != 0) {
            Some(pos) => bytes[pos..].to_vec(),
            None => vec![],
        };
        crate::proto::pseudosettle::Payment { amount: trimmed }
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        let amount = if proto.amount.is_empty() {
            U256::ZERO
        } else {
            U256::from_be_slice(&proto.amount)
        };
        Ok(Self { amount })
    }
}

pub type PaymentCodec = Codec<Payment, PseudosettleCodecError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaymentAck {
    pub amount: U256,
    pub timestamp: i64,
}

impl PaymentAck {
    pub fn new(amount: U256, timestamp: i64) -> Self {
        Self { amount, timestamp }
    }

    pub fn now(amount: U256) -> Self {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos() as i64;
        Self { amount, timestamp }
    }
}

impl ProtoMessage for PaymentAck {
    type Proto = crate::proto::pseudosettle::PaymentAck;
    type DecodeError = PseudosettleCodecError;

    fn into_proto(self) -> Self::Proto {
        let bytes = self.amount.to_be_bytes::<32>();
        let trimmed = match bytes.iter().position(|&b| b != 0) {
            Some(pos) => bytes[pos..].to_vec(),
            None => vec![],
        };
        crate::proto::pseudosettle::PaymentAck {
            amount: trimmed,
            timestamp: self.timestamp,
        }
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        let amount = if proto.amount.is_empty() {
            U256::ZERO
        } else {
            U256::from_be_slice(&proto.amount)
        };
        Ok(Self {
            amount,
            timestamp: proto.timestamp,
        })
    }
}

pub type PaymentAckCodec = Codec<PaymentAck, PseudosettleCodecError>;

#[cfg(test)]
mod tests {
    use super::*;
    use asynchronous_codec::{Decoder, Encoder};
    use bytes::BytesMut;

    #[test]
    fn test_payment_roundtrip() {
        let original = Payment::from_u64(13_500_000);
        let mut codec = PaymentCodec::new(1024);
        let mut buf = BytesMut::new();

        codec.encode(original.clone(), &mut buf).unwrap();
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_payment_zero() {
        let original = Payment::new(U256::ZERO);
        let mut codec = PaymentCodec::new(1024);
        let mut buf = BytesMut::new();

        codec.encode(original.clone(), &mut buf).unwrap();
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_payment_ack_roundtrip() {
        let original = PaymentAck::new(U256::from(1_000_000u64), 1234567890123456789);
        let mut codec = PaymentAckCodec::new(1024);
        let mut buf = BytesMut::new();

        codec.encode(original.clone(), &mut buf).unwrap();
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_payment_ack_now() {
        let amount = U256::from(500_000u64);
        let ack = PaymentAck::now(amount);
        assert_eq!(ack.amount, amount);
        assert!(ack.timestamp > 0);
    }
}
