//! Syn message codec for handshake protocol.

use libp2p::Multiaddr;
use vertex_net_codec::ProtoMessage;
use vertex_swarm_peer::deserialize_multiaddrs;

use crate::HandshakeError;

/// Syn message containing the observed multiaddr of the remote peer.
#[derive(Debug, Clone, PartialEq)]
pub struct Syn {
    observed_multiaddr: Multiaddr,
}

impl Syn {
    /// Create a new Syn message with the observed multiaddr.
    pub fn new(observed_multiaddr: Multiaddr) -> Self {
        Self { observed_multiaddr }
    }

    /// Returns the observed multiaddr.
    pub fn observed_multiaddr(&self) -> &Multiaddr {
        &self.observed_multiaddr
    }
}

impl ProtoMessage for Syn {
    type Proto = crate::proto::handshake::Syn;
    type EncodeError = std::convert::Infallible;
    type DecodeError = HandshakeError;

    fn into_proto(self) -> Result<Self::Proto, Self::EncodeError> {
        Ok(crate::proto::handshake::Syn {
            observed_multiaddr: self.observed_multiaddr.to_vec(),
        })
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        let multiaddrs = deserialize_multiaddrs(&proto.observed_multiaddr)?;

        let multiaddr = multiaddrs
            .into_iter()
            .next()
            .ok_or(HandshakeError::MissingField("observed_multiaddr"))?;

        Ok(Self::new(multiaddr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_syn() -> Syn {
        Syn::new(Multiaddr::try_from("/ip4/127.0.0.1/tcp/1234").unwrap())
    }

    #[test]
    fn test_syn_proto_roundtrip() {
        let syn = create_test_syn();
        let proto_syn = syn.clone().into_proto().unwrap();
        let recovered_syn = Syn::from_proto(proto_syn).unwrap();

        assert_eq!(syn, recovered_syn);
        assert_eq!(syn.observed_multiaddr(), recovered_syn.observed_multiaddr());
    }

    #[test]
    fn test_syn_err_on_malformed_proto() {
        let mut proto_syn = create_test_syn().into_proto().unwrap();
        proto_syn.observed_multiaddr = vec![0x01, 0x02, 0x03];

        let result = Syn::from_proto(proto_syn);
        assert!(matches!(result, Err(HandshakeError::InvalidMultiaddr(_))));
    }
}
