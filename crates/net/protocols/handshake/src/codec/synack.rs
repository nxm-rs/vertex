//! SynAck message codec for handshake protocol.

use vertex_swarm_peer::SwarmPeer;
use vertex_net_codec::{ProtoMessage, ProtoMessageWithContext};

use super::error::{CodecError, HandshakeCodecDomainError};
use super::Syn;
use super::ack::{swarm_peer_from_proto, swarm_peer_to_proto, welcome_message_from_proto};

/// SynAck message containing both Syn echo and peer identity.
#[derive(Clone)]
pub struct SynAck {
    syn: Syn,
    swarm_peer: SwarmPeer,
    full_node: bool,
    welcome_message: String,
}

impl SynAck {
    /// Create a new SynAck message.
    pub fn new(
        syn: Syn,
        swarm_peer: SwarmPeer,
        full_node: bool,
        welcome_message: String,
    ) -> Self {
        Self {
            syn,
            swarm_peer,
            full_node,
            welcome_message,
        }
    }

    /// Returns the Syn component.
    pub fn syn(&self) -> &Syn {
        &self.syn
    }

    /// Returns the peer's Swarm identity.
    pub fn swarm_peer(&self) -> &SwarmPeer {
        &self.swarm_peer
    }

    /// Returns whether this is a full node.
    pub fn full_node(&self) -> bool {
        self.full_node
    }

    /// Returns the welcome message.
    pub fn welcome_message(&self) -> &str {
        &self.welcome_message
    }

    /// Consume and return the components.
    pub fn into_parts(self) -> (Syn, SwarmPeer, bool, String) {
        (self.syn, self.swarm_peer, self.full_node, self.welcome_message)
    }
}

impl ProtoMessageWithContext<u64> for SynAck {
    type Proto = crate::proto::handshake::SynAck;
    type DecodeError = CodecError;

    fn into_proto_with_context(self, network_id: &u64) -> Self::Proto {
        crate::proto::handshake::SynAck {
            syn: Some(self.syn.into_proto()),
            ack: Some(swarm_peer_to_proto(
                &self.swarm_peer,
                *network_id,
                self.full_node,
                &self.welcome_message,
            )),
        }
    }

    fn from_proto_with_context(proto: Self::Proto, expected_network_id: &u64) -> Result<Self, Self::DecodeError> {
        let syn = Syn::from_proto(
            proto.syn.ok_or_else(|| CodecError::domain(HandshakeCodecDomainError::MissingField("syn")))?
        )?;
        let proto_ack = proto.ack.ok_or_else(|| CodecError::domain(HandshakeCodecDomainError::MissingField("ack")))?;

        let swarm_peer = swarm_peer_from_proto(&proto_ack, *expected_network_id)?;
        let welcome_message = welcome_message_from_proto(&proto_ack)?;

        Ok(SynAck::new(syn, swarm_peer, proto_ack.full_node, welcome_message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use alloy_primitives::B256;
    use alloy_signer_local::PrivateKeySigner;
    use libp2p::Multiaddr;
    use vertex_net_codec::ProtocolCodecError;

    const TEST_NETWORK_ID: u64 = 1234567890;

    fn create_test_synack() -> SynAck {
        let syn = Syn::new(Multiaddr::try_from("/ip4/127.0.0.1/tcp/1234").unwrap());
        let signer = Arc::new(PrivateKeySigner::random());
        let multiaddr: Multiaddr = "/ip4/192.168.1.1/tcp/5678".parse().unwrap();
        let peer = SwarmPeer::with_signer(
            vec![multiaddr],
            B256::default(),
            TEST_NETWORK_ID,
            signer,
        ).unwrap();
        SynAck::new(syn, peer, true, "test".to_string())
    }

    #[test]
    fn test_synack_proto_roundtrip() {
        let synack = create_test_synack();

        // Convert SynAck to proto with context
        let proto_synack = synack.clone().into_proto_with_context(&TEST_NETWORK_ID);

        // Convert proto back to SynAck
        let recovered_synack = SynAck::from_proto_with_context(proto_synack, &TEST_NETWORK_ID).unwrap();

        // Verify equality
        assert_eq!(synack.syn(), recovered_synack.syn());
        assert_eq!(synack.swarm_peer(), recovered_synack.swarm_peer());
        assert_eq!(synack.full_node(), recovered_synack.full_node());
        assert_eq!(synack.welcome_message(), recovered_synack.welcome_message());
    }

    #[test]
    fn test_synack_err_on_malformed_proto() {
        let synack = create_test_synack();
        let proto_synack = synack.into_proto_with_context(&TEST_NETWORK_ID);

        // Test missing syn
        let mut modified = proto_synack.clone();
        modified.syn = None;
        let result = SynAck::from_proto_with_context(modified, &TEST_NETWORK_ID);
        assert!(matches!(
            result,
            Err(ProtocolCodecError::Domain(HandshakeCodecDomainError::MissingField("syn")))
        ));

        // Test missing ack
        let mut modified = proto_synack.clone();
        modified.ack = None;
        let result = SynAck::from_proto_with_context(modified, &TEST_NETWORK_ID);
        assert!(matches!(
            result,
            Err(ProtocolCodecError::Domain(HandshakeCodecDomainError::MissingField("ack")))
        ));
    }
}
