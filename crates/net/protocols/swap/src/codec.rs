//! Codec for SWAP protocol messages.
//!
//! Provides separate typed codecs:
//! - `EmitChequeCodec` - Encodes/decodes `EmitCheque` messages
//! - `HandshakeCodec` - Encodes/decodes `Handshake` messages
//!
//! Cheques are serialized as JSON, matching Bee's format for interoperability.

use alloy_primitives::Address;
use vertex_bandwidth_chequebook::SignedCheque;
use vertex_net_codec::{Codec, ProtoMessage, ProtocolCodecError};

/// Domain-specific errors for SWAP protocol.
#[derive(Debug, thiserror::Error)]
pub enum SwapError {
    #[error("JSON error: {0}")]
    Json(String),
}

impl From<serde_json::Error> for SwapError {
    fn from(error: serde_json::Error) -> Self {
        SwapError::Json(error.to_string())
    }
}

/// Error type for swap codec operations.
pub type SwapCodecError = ProtocolCodecError<SwapError>;

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
    type Proto = crate::proto::swap::EmitCheque;
    type DecodeError = SwapCodecError;

    fn into_proto(self) -> Self::Proto {
        let cheque_json = serde_json::to_vec(&self.cheque).expect("SignedCheque serializes");
        crate::proto::swap::EmitCheque { cheque: cheque_json }
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        let cheque: SignedCheque = serde_json::from_slice(&proto.cheque)
            .map_err(|e| SwapCodecError::domain(SwapError::from(e)))?;
        Ok(Self { cheque })
    }
}

pub type EmitChequeCodec = Codec<EmitCheque, SwapCodecError>;

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
    type Proto = crate::proto::swap::Handshake;
    type DecodeError = SwapCodecError;

    fn into_proto(self) -> Self::Proto {
        crate::proto::swap::Handshake {
            beneficiary: self.beneficiary.as_slice().to_vec(),
        }
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        if proto.beneficiary.len() != 20 {
            return Err(SwapCodecError::protocol(format!(
                "invalid beneficiary length: expected 20, got {}",
                proto.beneficiary.len()
            )));
        }
        let beneficiary = Address::from_slice(&proto.beneficiary);
        Ok(Self { beneficiary })
    }
}

pub type HandshakeCodec = Codec<Handshake, SwapCodecError>;

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;
    use asynchronous_codec::{Decoder, Encoder};
    use bytes::{Bytes, BytesMut};
    use vertex_bandwidth_chequebook::{Cheque, ChequeExt};

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
