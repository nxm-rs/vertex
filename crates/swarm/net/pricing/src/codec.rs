//! Codec for pricing protocol messages.
//!
//! # Wire Format
//!
//! The payment threshold is encoded as trimmed big-endian bytes (leading zero
//! bytes removed).

use alloy_primitives::U256;
use vertex_net_codec::{Codec, ProtoMessage, decode_u256_be, encode_u256_be};

use crate::error::PricingError;

/// Codec for pricing protocol messages.
pub(crate) type PricingCodec = Codec<AnnouncePaymentThreshold, PricingError>;

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
    type Proto = vertex_swarm_net_proto::pricing::AnnouncePaymentThreshold;
    type EncodeError = std::convert::Infallible;
    type DecodeError = PricingError;

    fn into_proto(self) -> Result<Self::Proto, Self::EncodeError> {
        Ok(vertex_swarm_net_proto::pricing::AnnouncePaymentThreshold {
            payment_threshold: encode_u256_be(self.payment_threshold),
        })
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        Ok(Self {
            payment_threshold: decode_u256_be(&proto.payment_threshold)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_net_codec::assert_proto_roundtrip;

    #[test]
    fn test_roundtrip() {
        assert_proto_roundtrip!(AnnouncePaymentThreshold::from_u64(13_500_000));
    }

    #[test]
    fn test_zero_threshold() {
        let original = AnnouncePaymentThreshold::new(U256::ZERO);
        let proto = original.clone().into_proto().unwrap();
        assert!(proto.payment_threshold.is_empty());
        let decoded = AnnouncePaymentThreshold::from_proto(proto).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_large_threshold() {
        let original = AnnouncePaymentThreshold::new(U256::MAX);
        let proto = original.clone().into_proto().unwrap();
        assert_eq!(proto.payment_threshold.len(), 32);
        let decoded = AnnouncePaymentThreshold::from_proto(proto).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_small_value_minimal_bytes() {
        let original = AnnouncePaymentThreshold::from_u64(256);
        let proto = original.clone().into_proto().unwrap();
        assert_eq!(proto.payment_threshold, vec![0x01, 0x00]);
    }

    #[test]
    fn test_oversized_threshold_rejected() {
        let proto = vertex_swarm_net_proto::pricing::AnnouncePaymentThreshold {
            payment_threshold: vec![0xff; 33],
        };
        assert!(matches!(
            AnnouncePaymentThreshold::from_proto(proto),
            Err(PricingError::InvalidThreshold(_))
        ));
    }

    #[test]
    fn test_leading_zero_threshold_rejected() {
        let proto = vertex_swarm_net_proto::pricing::AnnouncePaymentThreshold {
            payment_threshold: vec![0x00, 0x01],
        };
        assert!(matches!(
            AnnouncePaymentThreshold::from_proto(proto),
            Err(PricingError::InvalidThreshold(_))
        ));
    }

    /// Replay the committed fuzz seeds through the same decode path the
    /// `pricing_decode` fuzz target drives, so the stable test gate proves
    /// the seeds stay panic-free without the fuzzer.
    #[test]
    fn seed_replay_pricing_decode() {
        let seed_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../fuzz/seeds/pricing_decode");
        let mut replayed = 0usize;
        for entry in std::fs::read_dir(&seed_dir)
            .unwrap_or_else(|e| panic!("seed dir {} must exist: {e}", seed_dir.display()))
        {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let data = std::fs::read(&path).unwrap();

            let announce = crate::fuzz::decode_announce(&data);
            if name.starts_with("valid-") {
                assert!(announce.is_some(), "seed {name} must decode");
            } else if name.starts_with("invalid-") || name.starts_with("crash-") {
                assert!(announce.is_none(), "seed {name} must stay an Err");
            } else {
                panic!("seed {name} matches no known prefix");
            }
            replayed += 1;
        }
        assert!(
            replayed >= 5,
            "expected at least the 5 curated seeds, found {replayed}"
        );
    }
}

// Stable pins of the round-trip invariant, drawing the message through the
// shared `Arbitrary` impl so the proptest suite and the fuzz round-trip
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
        fn announce_roundtrips(announce in arb::<AnnouncePaymentThreshold>()) {
            prop_assert_proto_roundtrip!(announce);
        }
    }
}
