//! SynAck message encoding/decoding for handshake protocol.

use libp2p::Multiaddr;
use vertex_swarm_peer::{SwarmNodeType, SwarmPeer};

use super::ack::{
    encode_ack, node_type_from_wire, swarm_peer_from_proto, welcome_message_from_proto,
};
use super::syn_msg::{decode_syn, encode_syn};
use crate::HandshakeError;

/// Decode a SynAck proto message, returning validated components.
pub(crate) fn decode_synack(
    proto: vertex_swarm_net_proto::handshake::SynAck,
    expected_network_id: u64,
) -> Result<(Multiaddr, SwarmPeer, SwarmNodeType, String), HandshakeError> {
    let observed = decode_syn(proto.syn.ok_or(HandshakeError::MissingField("syn"))?)?;

    let proto_ack = proto.ack.ok_or(HandshakeError::MissingField("ack"))?;
    let swarm_peer = swarm_peer_from_proto(&proto_ack, expected_network_id)?;
    let welcome_message = welcome_message_from_proto(&proto_ack)?;
    let node_type = node_type_from_wire(proto_ack.storer);

    Ok((observed, swarm_peer, node_type, welcome_message))
}

/// Encode components into a SynAck proto message.
pub(crate) fn encode_synack(
    observed: &Multiaddr,
    peer: &SwarmPeer,
    node_type: SwarmNodeType,
    welcome_message: &str,
    network_id: u64,
) -> vertex_swarm_net_proto::handshake::SynAck {
    vertex_swarm_net_proto::handshake::SynAck {
        syn: Some(encode_syn(observed)),
        ack: Some(encode_ack(peer, node_type, welcome_message, network_id)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_api::SwarmSpec;
    use vertex_swarm_identity::Identity;
    use vertex_swarm_test_utils::test_spec_isolated as test_spec;

    fn create_test_data() -> (Multiaddr, SwarmPeer, u64) {
        let spec = test_spec();
        let identity = Identity::random(spec.clone(), SwarmNodeType::Storer);
        let observed: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();
        let peer_addr: Multiaddr = "/ip4/192.168.1.1/tcp/5678".parse().unwrap();
        let peer = SwarmPeer::from_identity(&identity, vec![peer_addr]).unwrap();
        (observed, peer, spec.network_id())
    }

    #[test]
    fn test_synack_roundtrip() {
        let (observed, peer, network_id) = create_test_data();
        let node_type = SwarmNodeType::Storer;
        let welcome = "test";

        let proto = encode_synack(&observed, &peer, node_type, welcome, network_id);
        let (dec_observed, dec_peer, dec_type, dec_welcome) =
            decode_synack(proto, network_id).unwrap();

        assert_eq!(observed, dec_observed);
        assert_eq!(peer, dec_peer);
        assert_eq!(node_type, dec_type);
        assert_eq!(welcome, dec_welcome);
    }

    #[test]
    fn test_synack_missing_syn() {
        let (_, peer, network_id) = create_test_data();
        let mut proto = encode_synack(
            &"/ip4/127.0.0.1/tcp/1234".parse().unwrap(),
            &peer,
            SwarmNodeType::Client,
            "test",
            network_id,
        );
        proto.syn = None;

        let result = decode_synack(proto, network_id);
        assert!(matches!(result, Err(HandshakeError::MissingField("syn"))));
    }

    #[test]
    fn test_synack_missing_ack() {
        let (_, peer, network_id) = create_test_data();
        let mut proto = encode_synack(
            &"/ip4/127.0.0.1/tcp/1234".parse().unwrap(),
            &peer,
            SwarmNodeType::Client,
            "test",
            network_id,
        );
        proto.ack = None;

        let result = decode_synack(proto, network_id);
        assert!(matches!(result, Err(HandshakeError::MissingField("ack"))));
    }
}
