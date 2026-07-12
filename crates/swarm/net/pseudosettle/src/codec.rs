//! Codec for pseudosettle protocol messages.
//!
//! Provides separate typed codecs for request and response:
//! - `PaymentCodec` - Encodes/decodes `Payment` messages only
//! - `PaymentAckCodec` - Encodes/decodes `PaymentAck` messages only
//!
//! Amounts are encoded as trimmed big-endian bytes (leading zero bytes
//! removed).

use alloy_primitives::U256;
use vertex_net_codec::{
    Codec, ProtoMessage, current_unix_timestamp_nanos, decode_u256_be, encode_u256_be,
};

use crate::error::PseudosettleError;

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
    type Proto = vertex_swarm_net_proto::pseudosettle::Payment;
    type EncodeError = std::convert::Infallible;
    type DecodeError = PseudosettleError;

    fn into_proto(self) -> Result<Self::Proto, Self::EncodeError> {
        Ok(vertex_swarm_net_proto::pseudosettle::Payment {
            amount: encode_u256_be(self.amount),
        })
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        Ok(Self {
            amount: decode_u256_be(&proto.amount)?,
        })
    }
}

pub(crate) type PaymentCodec = Codec<Payment, PseudosettleError>;

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
        Self {
            amount,
            timestamp: current_unix_timestamp_nanos(),
        }
    }
}

impl ProtoMessage for PaymentAck {
    type Proto = vertex_swarm_net_proto::pseudosettle::PaymentAck;
    type EncodeError = std::convert::Infallible;
    type DecodeError = PseudosettleError;

    fn into_proto(self) -> Result<Self::Proto, Self::EncodeError> {
        Ok(vertex_swarm_net_proto::pseudosettle::PaymentAck {
            amount: encode_u256_be(self.amount),
            timestamp: self.timestamp,
        })
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        Ok(Self {
            amount: decode_u256_be(&proto.amount)?,
            timestamp: proto.timestamp,
        })
    }
}

pub(crate) type PaymentAckCodec = Codec<PaymentAck, PseudosettleError>;

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

    #[test]
    fn test_oversized_amount_rejected() {
        let proto = vertex_swarm_net_proto::pseudosettle::Payment {
            amount: vec![0xff; 33],
        };
        assert!(matches!(
            Payment::from_proto(proto),
            Err(PseudosettleError::InvalidAmount(_))
        ));
    }

    #[test]
    fn test_leading_zero_amount_rejected() {
        let proto = vertex_swarm_net_proto::pseudosettle::PaymentAck {
            amount: vec![0x00, 0x01],
            timestamp: 1,
        };
        assert!(matches!(
            PaymentAck::from_proto(proto),
            Err(PseudosettleError::InvalidAmount(_))
        ));
    }

    /// Replay the committed fuzz seeds through the same decode paths the
    /// `pseudosettle_decode` fuzz target drives, so the stable test gate
    /// proves the seeds stay panic-free without the fuzzer.
    #[test]
    fn seed_replay_pseudosettle_decode() {
        let seed_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../fuzz/seeds/pseudosettle_decode");
        let mut replayed = 0usize;
        for entry in std::fs::read_dir(&seed_dir)
            .unwrap_or_else(|e| panic!("seed dir {} must exist: {e}", seed_dir.display()))
        {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let data = std::fs::read(&path).unwrap();

            let payment = crate::fuzz::decode_payment(&data);
            let ack = crate::fuzz::decode_payment_ack(&data);

            if name.starts_with("valid-payment-") {
                assert!(payment.is_some(), "seed {name} must decode as a payment");
            } else if name.starts_with("valid-ack-") {
                assert!(ack.is_some(), "seed {name} must decode as an ack");
            } else if name.starts_with("invalid-") || name.starts_with("crash-") {
                assert!(payment.is_none(), "seed {name} must stay a payment Err");
                assert!(ack.is_none(), "seed {name} must stay an ack Err");
            }
            replayed += 1;
        }
        assert!(
            replayed >= 6,
            "expected at least the 6 curated seeds, found {replayed}"
        );
    }
}

// Stable pins of the round-trip invariant, drawing both messages through the
// shared `Arbitrary` impls so the proptest suite and the fuzz round-trip
// target drive one construction path.
#[cfg(test)]
mod proptests {
    use proptest::prelude::*;
    use proptest_arbitrary_interop::arb;
    use vertex_net_codec::prop_assert_proto_roundtrip;

    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn payment_roundtrips(payment in arb::<Payment>()) {
            prop_assert_proto_roundtrip!(payment);
        }

        #[test]
        fn payment_ack_roundtrips(ack in arb::<PaymentAck>()) {
            prop_assert_proto_roundtrip!(ack);
        }
    }
}
