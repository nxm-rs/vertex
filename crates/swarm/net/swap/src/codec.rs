//! Codec for SWAP protocol messages.
//!
//! Provides separate typed codecs:
//! - `EmitChequeCodec` - Encodes/decodes `EmitCheque` messages
//! - `HandshakeCodec` - Encodes/decodes `Handshake` messages
//!
//! The cheque rides as a JSON object in a protobuf `bytes` field. It is encoded
//! and decoded with `serde_json` directly over [`SignedCheque`]; the JSON is
//! transport-only (the signature is EIP-712 over the cheque fields, not the JSON
//! bytes) and is slated for protobuf replacement, tracked in issue #183.

use alloy_primitives::Address;
use vertex_net_codec::{Codec, ProtoMessage};
use vertex_swarm_bandwidth_chequebook::SignedCheque;

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
    #![allow(clippy::unwrap_used)]
    use super::*;
    use alloy_primitives::U256;
    use asynchronous_codec::{Decoder, Encoder};
    use bytes::{Bytes, BytesMut};
    use vertex_swarm_bandwidth_chequebook::{Cheque, ChequeExt};

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
}
