use crate::HandshakeError;

use super::{Ack, Syn};

#[derive(Debug, Clone)]
pub struct SynAck<const N: u64> {
    pub(crate) syn: Syn<N>,
    pub(crate) ack: Ack<N>,
}

impl<const N: u64> TryFrom<crate::proto::handshake::SynAck> for SynAck<N> {
    type Error = HandshakeError;

    fn try_from(value: crate::proto::handshake::SynAck) -> Result<Self, Self::Error> {
        Ok(Self {
            syn: value
                .Syn
                .ok_or_else(|| HandshakeError::MissingField("syn"))?
                .try_into()?,
            ack: value
                .Ack
                .ok_or_else(|| HandshakeError::MissingField("ack"))?
                .try_into()?,
        })
    }
}

impl<const N: u64> From<SynAck<N>> for crate::proto::handshake::SynAck {
    fn from(value: SynAck<N>) -> Self {
        crate::proto::handshake::SynAck {
            Syn: Some(value.syn.into()),
            Ack: Some(value.ack.into()),
        }
    }
}
