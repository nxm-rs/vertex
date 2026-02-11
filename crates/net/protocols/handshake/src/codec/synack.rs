//! SynAck message codec for handshake protocol.

use vertex_net_codec::{ProtoMessage, ProtoMessageWithContext};
use vertex_swarm_peer::{SwarmNodeType, SwarmPeer};

use super::Syn;
use super::ack::{swarm_peer_from_proto, swarm_peer_to_proto, welcome_message_from_proto, node_type_from_wire};
use crate::HandshakeError;

/// SynAck message containing both Syn echo and peer identity.
#[derive(Clone)]
pub struct SynAck {
    syn: Syn,
    swarm_peer: SwarmPeer,
    node_type: SwarmNodeType,
    welcome_message: String,
}

impl SynAck {
    /// Create a new SynAck message.
    pub fn new(syn: Syn, swarm_peer: SwarmPeer, node_type: SwarmNodeType, welcome_message: String) -> Self {
        Self {
            syn,
            swarm_peer,
            node_type,
            welcome_message,
        }
    }

    pub fn syn(&self) -> &Syn {
        &self.syn
    }

    pub fn swarm_peer(&self) -> &SwarmPeer {
        &self.swarm_peer
    }

    /// Returns the peer's node type.
    pub fn node_type(&self) -> SwarmNodeType {
        self.node_type
    }

    pub fn welcome_message(&self) -> &str {
        &self.welcome_message
    }

    /// Consume and return the components.
    pub fn into_parts(self) -> (Syn, SwarmPeer, SwarmNodeType, String) {
        (
            self.syn,
            self.swarm_peer,
            self.node_type,
            self.welcome_message,
        )
    }
}

impl ProtoMessageWithContext<u64> for SynAck {
    type Proto = crate::proto::handshake::SynAck;
    type EncodeError = std::convert::Infallible;
    type DecodeError = HandshakeError;

    fn into_proto_with_context(self, network_id: &u64) -> Result<Self::Proto, Self::EncodeError> {
        Ok(crate::proto::handshake::SynAck {
            syn: Some(self.syn.into_proto()?),
            ack: Some(swarm_peer_to_proto(
                &self.swarm_peer,
                *network_id,
                self.node_type,
                &self.welcome_message,
            )),
        })
    }

    fn from_proto_with_context(
        proto: Self::Proto,
        expected_network_id: &u64,
    ) -> Result<Self, Self::DecodeError> {
        let syn = Syn::from_proto(
            proto
                .syn
                .ok_or(HandshakeError::MissingField("syn"))?,
        )?;
        let proto_ack = proto
            .ack
            .ok_or(HandshakeError::MissingField("ack"))?;

        let swarm_peer = swarm_peer_from_proto(&proto_ack, *expected_network_id)?;
        let welcome_message = welcome_message_from_proto(&proto_ack)?;
        // Wire format uses bool for backwards compatibility
        let node_type = node_type_from_wire(proto_ack.storer);

        Ok(SynAck::new(
            syn,
            swarm_peer,
            node_type,
            welcome_message,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::Multiaddr;
    use vertex_swarm_api::SwarmSpec;
    use vertex_swarm_identity::Identity;
    use vertex_swarm_peer::SwarmNodeType;
    use vertex_swarm_test_utils::test_spec_isolated as test_spec;

    fn create_test_synack() -> (SynAck, u64) {
        let spec = test_spec();
        let identity = Identity::random(spec.clone(), SwarmNodeType::Storer);
        let syn = Syn::new(Multiaddr::try_from("/ip4/127.0.0.1/tcp/1234").unwrap());
        let multiaddr: Multiaddr = "/ip4/192.168.1.1/tcp/5678".parse().unwrap();
        let peer = SwarmPeer::from_identity(&identity, vec![multiaddr]).unwrap();
        (
            SynAck::new(syn, peer, SwarmNodeType::Storer, "test".to_string()),
            spec.network_id(),
        )
    }

    #[test]
    fn test_synack_proto_roundtrip() {
        let (synack, network_id) = create_test_synack();

        let proto_synack = synack.clone().into_proto_with_context(&network_id).unwrap();
        let recovered_synack =
            SynAck::from_proto_with_context(proto_synack, &network_id).unwrap();

        assert_eq!(synack.syn(), recovered_synack.syn());
        assert_eq!(synack.swarm_peer(), recovered_synack.swarm_peer());
        assert_eq!(synack.node_type(), recovered_synack.node_type());
        assert_eq!(synack.welcome_message(), recovered_synack.welcome_message());
    }

    #[test]
    fn test_synack_err_on_malformed_proto() {
        let (synack, network_id) = create_test_synack();
        let proto_synack = synack.into_proto_with_context(&network_id).unwrap();

        // Test missing syn
        let mut modified = proto_synack.clone();
        modified.syn = None;
        let result = SynAck::from_proto_with_context(modified, &network_id);
        assert!(matches!(
            result,
            Err(HandshakeError::MissingField("syn"))
        ));

        // Test missing ack
        let mut modified = proto_synack.clone();
        modified.ack = None;
        let result = SynAck::from_proto_with_context(modified, &network_id);
        assert!(matches!(
            result,
            Err(HandshakeError::MissingField("ack"))
        ));
    }
}
