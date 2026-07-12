//! Codec for SWAP protocol messages.
//!
//! Provides separate typed codecs:
//! - `EmitChequeCodec` - Encodes/decodes `EmitCheque` messages
//! - `HandshakeCodec` - Encodes/decodes `Handshake` messages
//!
//! The cheque rides as a JSON object in a protobuf `bytes` field. It is encoded
//! and decoded with `serde_json` directly over [`SignedCheque`]; the JSON is
//! transport-only (the signature is EIP-712 over the cheque fields, not the JSON
//! bytes).

use alloy_primitives::Address;
use vertex_net_codec::{Codec, ProtoMessage};
use vertex_swarm_accounting_chequebook::SignedCheque;

use crate::error::SwapError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitCheque {
    pub cheque: SignedCheque,
}

impl EmitCheque {
    pub fn new(cheque: SignedCheque) -> Self {
        Self { cheque }
    }
}

impl ProtoMessage for EmitCheque {
    type Proto = vertex_swarm_net_proto::swap::EmitCheque;
    type EncodeError = SwapError;
    type DecodeError = SwapError;

    fn into_proto(self) -> Result<Self::Proto, Self::EncodeError> {
        let cheque = serde_json::to_vec(&self.cheque).map_err(SwapError::ChequeEncode)?;
        Ok(vertex_swarm_net_proto::swap::EmitCheque { cheque })
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        let cheque = serde_json::from_slice(&proto.cheque).map_err(SwapError::ChequeDecode)?;
        Ok(Self { cheque })
    }
}

pub type EmitChequeCodec = Codec<EmitCheque, SwapError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handshake {
    pub beneficiary: Address,
}

impl Handshake {
    pub fn new(beneficiary: Address) -> Self {
        Self { beneficiary }
    }
}

impl ProtoMessage for Handshake {
    type Proto = vertex_swarm_net_proto::swap::Handshake;
    type EncodeError = std::convert::Infallible;
    type DecodeError = SwapError;

    fn into_proto(self) -> Result<Self::Proto, Self::EncodeError> {
        Ok(vertex_swarm_net_proto::swap::Handshake {
            beneficiary: self.beneficiary.as_slice().to_vec(),
        })
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        if proto.beneficiary.len() != 20 {
            return Err(SwapError::InvalidBeneficiaryLength(proto.beneficiary.len()));
        }
        let beneficiary = Address::from_slice(&proto.beneficiary);
        Ok(Self { beneficiary })
    }
}

pub type HandshakeCodec = Codec<Handshake, SwapError>;

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;
    use asynchronous_codec::{Decoder, Encoder};
    use bytes::{Bytes, BytesMut};
    use vertex_swarm_accounting_chequebook::{Cheque, ChequeExt};

    fn test_signed_cheque() -> SignedCheque {
        let cheque = Cheque::new(
            Address::repeat_byte(0x01),
            Address::repeat_byte(0x02),
            U256::from(1_000_000u64),
        );
        SignedCheque::new(cheque, Bytes::from(vec![0u8; 65]))
    }

    #[test]
    fn test_emit_cheque_roundtrip() {
        let original = EmitCheque::new(test_signed_cheque());
        let mut codec = EmitChequeCodec::new(4096);
        let mut buf = BytesMut::new();

        codec.encode(original.clone(), &mut buf).unwrap();
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_handshake_roundtrip() {
        let original = Handshake::new(Address::repeat_byte(0x42));
        let mut codec = HandshakeCodec::new(1024);
        let mut buf = BytesMut::new();

        codec.encode(original.clone(), &mut buf).unwrap();
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(original, decoded);
    }

    /// Replay the committed fuzz seeds through the same decode paths the
    /// `swap_decode` fuzz target drives, so the stable test gate proves the
    /// seeds stay panic-free without the fuzzer.
    #[test]
    fn seed_replay_swap_decode() {
        let seed_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../fuzz/seeds/swap_decode");
        let mut replayed = 0usize;
        for entry in std::fs::read_dir(&seed_dir)
            .unwrap_or_else(|e| panic!("seed dir {} must exist: {e}", seed_dir.display()))
        {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let data = std::fs::read(&path).unwrap();

            let cheque = crate::fuzz::decode_emit_cheque(&data);
            let handshake = crate::fuzz::decode_handshake(&data);

            if name.starts_with("valid-cheque-") {
                assert!(cheque.is_some(), "seed {name} must decode as a cheque");
            } else if name.starts_with("valid-handshake-") {
                assert!(
                    handshake.is_some(),
                    "seed {name} must decode as a handshake"
                );
            } else if name.starts_with("invalid-cheque-") {
                assert!(cheque.is_none(), "seed {name} must stay a cheque Err");
            } else if name.starts_with("invalid-handshake-") {
                assert!(handshake.is_none(), "seed {name} must stay a handshake Err");
            } else {
                panic!("seed {name} matches no known prefix");
            }
            replayed += 1;
        }
        assert!(
            replayed >= 7,
            "expected at least the 7 curated seeds, found {replayed}"
        );
    }
}

// Stable pins of the round-trip invariant, drawing both messages through the
// shared `Arbitrary` impls (any 65 signature bytes round-trip: the codec
// carries them opaquely, validity is the chequebook layer's concern) so the
// proptest suite and the fuzz round-trip target drive one construction path.
#[cfg(test)]
mod proptests {
    use proptest::prelude::*;
    use proptest_arbitrary_interop::arb;
    use vertex_net_codec::prop_assert_proto_roundtrip;

    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn emit_cheque_roundtrips(cheque in arb::<EmitCheque>()) {
            prop_assert_proto_roundtrip!(cheque);
        }

        #[test]
        fn handshake_roundtrips(handshake in arb::<Handshake>()) {
            prop_assert_proto_roundtrip!(handshake);
        }
    }
}
