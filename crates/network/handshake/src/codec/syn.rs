use libp2p::Multiaddr;

use super::CodecError;

#[derive(Debug, Clone)]
pub struct Syn<const N: u64> {
    pub(crate) observed_underlay: Multiaddr,
}

impl<const N: u64> TryFrom<crate::proto::handshake::Syn> for Syn<N> {
    type Error = CodecError;

    fn try_from(value: crate::proto::handshake::Syn) -> Result<Self, Self::Error> {
        Ok(Self {
            observed_underlay: Multiaddr::try_from(value.ObservedUnderlay)?,
        })
    }
}

impl<const N: u64> Into<crate::proto::handshake::Syn> for Syn<N> {
    fn into(self) -> crate::proto::handshake::Syn {
        crate::proto::handshake::Syn {
            ObservedUnderlay: self.observed_underlay.to_vec(),
        }
    }
}
