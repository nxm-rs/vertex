//! Ack message encoding/decoding for handshake protocol.

use nectar_primitives::SwarmAddress;
use tracing::debug;
use vertex_swarm_peer::{SwarmNodeType, SwarmPeer};

use crate::HandshakeError;
use crate::MAX_WELCOME_MESSAGE_CHARS;

/// Decode an Ack proto message, validating network_id and returning components.
pub(crate) fn decode_ack(
    proto: vertex_swarm_net_proto::handshake::Ack,
    expected_network_id: u64,
) -> Result<(SwarmPeer, SwarmNodeType, String), HandshakeError> {
    let swarm_peer = swarm_peer_from_proto(&proto, expected_network_id)?;
    let welcome_message = welcome_message_from_proto(&proto)?;
    let node_type = node_type_from_wire(proto.storer);
    Ok((swarm_peer, node_type, welcome_message))
}

/// Encode identity into an Ack proto message.
pub(crate) fn encode_ack(
    peer: &SwarmPeer,
    node_type: SwarmNodeType,
    welcome_message: &str,
    network_id: u64,
) -> vertex_swarm_net_proto::handshake::Ack {
    vertex_swarm_net_proto::handshake::Ack {
        peer: Some(vertex_swarm_net_proto::handshake::PeerAddress {
            multiaddrs: peer.serialize_multiaddrs(),
            signature: peer.signature().as_bytes().to_vec(),
            overlay: peer.overlay().to_vec(),
        }),
        network_id,
        storer: node_type_to_wire(node_type),
        nonce: peer.nonce().to_vec(),
        welcome_message: welcome_message.to_string(),
    }
}

/// Convert wire format bool to SwarmNodeType.
///
/// Wire format uses `bool storer` for backwards compatibility with existing peers.
/// Bootnodes don't participate in handshake (topology-only).
pub(crate) fn node_type_from_wire(storer: bool) -> SwarmNodeType {
    if storer {
        SwarmNodeType::Storer
    } else {
        SwarmNodeType::Client
    }
}

fn node_type_to_wire(node_type: SwarmNodeType) -> bool {
    node_type.requires_storage()
}

pub(crate) fn swarm_peer_from_proto(
    value: &vertex_swarm_net_proto::handshake::Ack,
    expected_network_id: u64,
) -> Result<SwarmPeer, HandshakeError> {
    if value.network_id != expected_network_id {
        return Err(HandshakeError::NetworkIdMismatch);
    }

    let peer_address = value
        .peer
        .as_ref()
        .ok_or(HandshakeError::MissingField("peer"))?;

    let overlay = SwarmAddress::from_slice(peer_address.overlay.as_slice())
        .inspect_err(|e| debug!(error = ?e, "invalid overlay in handshake ack"))
        .map_err(|_| HandshakeError::InvalidOverlay)?;

    let nonce = value.nonce.as_slice().try_into()?;
    let signature = peer_address.signature.as_slice().try_into()?;

    SwarmPeer::from_signed(
        &peer_address.multiaddrs,
        signature,
        overlay,
        nonce,
        value.network_id,
        true,
    )
    .map_err(HandshakeError::from)
}

pub(crate) fn welcome_message_from_proto(
    value: &vertex_swarm_net_proto::handshake::Ack,
) -> Result<String, HandshakeError> {
    let char_count = value.welcome_message.chars().count();
    if char_count > MAX_WELCOME_MESSAGE_CHARS {
        return Err(HandshakeError::FieldTooLong {
            field: "welcome_message",
            max: MAX_WELCOME_MESSAGE_CHARS,
            actual: char_count,
        });
    }
    Ok(value.welcome_message.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::Multiaddr;
    use vertex_swarm_api::SwarmSpec;
    use vertex_swarm_identity::Identity;
    use vertex_swarm_test_utils::test_spec_isolated as test_spec;

    fn create_test_peer() -> SwarmPeer {
        let spec = test_spec();
        let identity = Identity::random(spec.clone(), SwarmNodeType::Storer);
        let multiaddr: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();
        SwarmPeer::from_identity(&identity, vec![multiaddr]).expect("should create peer")
    }

    #[test]
    fn test_ack_roundtrip() {
        let spec = test_spec();
        let peer = create_test_peer();
        let node_type = SwarmNodeType::Storer;
        let welcome = "hello";

        let proto = encode_ack(&peer, node_type, welcome, spec.network_id());
        let (decoded_peer, decoded_type, decoded_welcome) =
            decode_ack(proto, spec.network_id()).unwrap();

        assert_eq!(peer, decoded_peer);
        assert_eq!(node_type, decoded_type);
        assert_eq!(welcome, decoded_welcome);
    }

    #[test]
    fn test_wire_format_backwards_compat() {
        assert_eq!(node_type_from_wire(true), SwarmNodeType::Storer);
        assert_eq!(node_type_from_wire(false), SwarmNodeType::Client);
        assert!(node_type_to_wire(SwarmNodeType::Storer));
        assert!(!node_type_to_wire(SwarmNodeType::Client));
        assert!(!node_type_to_wire(SwarmNodeType::Bootnode));
    }

    #[test]
    fn test_network_id_mismatch() {
        let spec = test_spec();
        let peer = create_test_peer();
        let proto = encode_ack(&peer, SwarmNodeType::Client, "hello", spec.network_id());

        let wrong_network_id = spec.network_id().wrapping_add(1);
        let result = decode_ack(proto, wrong_network_id);
        assert!(matches!(result, Err(HandshakeError::NetworkIdMismatch)));
    }

    #[test]
    fn test_missing_peer_field() {
        let spec = test_spec();
        let peer = create_test_peer();
        let mut proto = encode_ack(&peer, SwarmNodeType::Client, "test", spec.network_id());
        proto.peer = None;

        let result = decode_ack(proto, spec.network_id());
        assert!(matches!(result, Err(HandshakeError::MissingField("peer"))));
    }

    #[test]
    fn test_empty_multiaddrs_rejected() {
        let spec = test_spec();
        let peer = create_test_peer();
        let mut proto = encode_ack(&peer, SwarmNodeType::Client, "test", spec.network_id());

        if let Some(ref mut peer_addr) = proto.peer {
            peer_addr.multiaddrs = vec![];
        }

        let result = decode_ack(proto, spec.network_id());
        assert!(
            matches!(
                result,
                Err(HandshakeError::InvalidPeer(
                    vertex_swarm_peer::SwarmPeerError::NoMultiaddrs
                ))
            ),
            "expected NoMultiaddrs error, got: {:?}",
            result
        );
    }

    #[test]
    fn test_welcome_message_max_length() {
        let spec = test_spec();
        let peer = create_test_peer();

        // At max length - should succeed
        let max_message = "x".repeat(MAX_WELCOME_MESSAGE_CHARS);
        let proto = encode_ack(&peer, SwarmNodeType::Client, &max_message, spec.network_id());
        assert!(decode_ack(proto, spec.network_id()).is_ok());

        // Over max length - should fail
        let mut proto = encode_ack(&peer, SwarmNodeType::Client, "", spec.network_id());
        proto.welcome_message = "x".repeat(MAX_WELCOME_MESSAGE_CHARS + 1);
        assert!(matches!(
            decode_ack(proto, spec.network_id()),
            Err(HandshakeError::FieldTooLong { .. })
        ));
    }

    #[test]
    fn test_invalid_signature() {
        let spec = test_spec();
        let peer = create_test_peer();
        let mut proto = encode_ack(&peer, SwarmNodeType::Client, "test", spec.network_id());

        if let Some(ref mut peer_addr) = proto.peer {
            peer_addr.signature = vec![0u8; 65];
        }

        let result = decode_ack(proto, spec.network_id());
        assert!(matches!(
            result,
            Err(HandshakeError::InvalidPeer(
                vertex_swarm_peer::SwarmPeerError::InvalidSignature(_)
            ))
        ));
    }

    #[test]
    fn test_invalid_nonce_length() {
        let spec = test_spec();
        let peer = create_test_peer();
        let mut proto = encode_ack(&peer, SwarmNodeType::Client, "test", spec.network_id());
        proto.nonce = vec![0u8; 16]; // Wrong length

        let result = decode_ack(proto, spec.network_id());
        assert!(matches!(result, Err(HandshakeError::InvalidData(_))));
    }
}
