//! Ack message encoding/decoding for handshake protocol (15.0.0).

use nectar_primitives::{NetworkId, Nonce, SwarmAddress, Timestamp};
use tracing::debug;
use vertex_swarm_peer::{SwarmNodeType, SwarmPeer, SwarmPeerWire};

use crate::HandshakeError;
use crate::MAX_WELCOME_MESSAGE_CHARS;

/// Decode an `Ack` proto message, validating `network_id` and returning the
/// recovered peer record + node type + welcome message.
pub(crate) fn decode_ack(
    proto: vertex_swarm_net_proto::handshake::Ack,
    expected_network_id: NetworkId,
) -> Result<(SwarmPeer, SwarmNodeType, String), HandshakeError> {
    if proto.network_id != expected_network_id.get() {
        return Err(HandshakeError::NetworkIdMismatch);
    }
    let peer = swarm_peer_from_proto(proto.address.as_ref(), expected_network_id)?;
    let welcome_message = welcome_message_from_proto(&proto)?;
    let node_type = node_type_from_wire(proto.storer);
    Ok((peer, node_type, welcome_message))
}

/// Encode a `SwarmPeer` into an `Ack` proto message.
pub(crate) fn encode_ack(
    peer: &SwarmPeer,
    node_type: SwarmNodeType,
    welcome_message: &str,
    network_id: NetworkId,
) -> vertex_swarm_net_proto::handshake::Ack {
    vertex_swarm_net_proto::handshake::Ack {
        address: Some(encode_swarm_peer(peer)),
        network_id: network_id.get(),
        storer: node_type_to_wire(node_type),
        welcome_message: welcome_message.to_string(),
    }
}

/// Wire format uses `bool storer` for backwards compatibility with existing
/// peers; bootnodes do not participate in handshake.
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

/// Encode a `SwarmPeer` into the proto `SwarmPeer` message.
pub(crate) fn encode_swarm_peer(peer: &SwarmPeer) -> vertex_swarm_net_proto::handshake::SwarmPeer {
    vertex_swarm_net_proto::handshake::SwarmPeer {
        multiaddrs: peer.serialize_multiaddrs(),
        signature: peer.signature().as_bytes().to_vec(),
        overlay: peer.overlay().to_vec(),
        nonce: peer.nonce().as_slice().to_vec(),
        timestamp: peer.timestamp().get(),
        chequebook_address: peer
            .chequebook()
            .map(|a| a.as_slice().to_vec())
            .unwrap_or_default(),
    }
}

/// Decode a proto `SwarmPeer` into vertex's `SwarmPeer`, validating
/// signature, overlay and (when `now` is supplied) the clock-skew tolerance.
pub(crate) fn swarm_peer_from_proto(
    proto: Option<&vertex_swarm_net_proto::handshake::SwarmPeer>,
    network_id: NetworkId,
) -> Result<SwarmPeer, HandshakeError> {
    let proto = proto.ok_or(HandshakeError::MissingField("address"))?;

    let overlay = SwarmAddress::from_slice(proto.overlay.as_slice())
        .inspect_err(|e| debug!(error = ?e, "invalid overlay in handshake address"))
        .map_err(|_| HandshakeError::InvalidOverlay)?;

    let nonce_bytes: [u8; 32] = proto.nonce.as_slice().try_into()?;
    let nonce = Nonce::new(nonce_bytes);
    let signature = proto.signature.as_slice().try_into()?;
    let timestamp = Timestamp::from_seconds(proto.timestamp);

    let wire = SwarmPeerWire {
        multiaddrs_bytes: &proto.multiaddrs,
        signature,
        overlay,
        nonce,
        timestamp,
        chequebook_bytes: &proto.chequebook_address,
    };

    // Codec does no skew check; that's policy left to the session layer.
    SwarmPeer::parse(wire, network_id, None).map_err(HandshakeError::from)
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
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use libp2p::Multiaddr;
    use vertex_swarm_api::{SwarmIdentity, SwarmSpec};
    use vertex_swarm_identity::Identity;
    use vertex_swarm_test_utils::test_spec_isolated as test_spec;

    fn create_test_peer() -> SwarmPeer {
        let spec = test_spec();
        let identity = Identity::random(spec.clone(), SwarmNodeType::Storer);
        let multiaddr: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();
        let signer = identity.signer();
        SwarmPeer::sign(
            &*signer,
            vec![multiaddr],
            identity.overlay_address(),
            spec.network_id(),
            identity.nonce(),
            Timestamp::now(),
            None,
        )
        .expect("should sign peer")
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
    fn test_wire_format_storer_flag() {
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
        let wrong = NetworkId::from(spec.network_id().get().wrapping_add(1));
        let result = decode_ack(proto, wrong);
        assert!(matches!(result, Err(HandshakeError::NetworkIdMismatch)));
    }

    #[test]
    fn test_missing_address_field() {
        let spec = test_spec();
        let peer = create_test_peer();
        let mut proto = encode_ack(&peer, SwarmNodeType::Client, "test", spec.network_id());
        proto.address = None;
        let result = decode_ack(proto, spec.network_id());
        assert!(matches!(
            result,
            Err(HandshakeError::MissingField("address"))
        ));
    }

    #[test]
    fn test_welcome_message_max_length() {
        let spec = test_spec();
        let peer = create_test_peer();

        let max_message = "x".repeat(MAX_WELCOME_MESSAGE_CHARS);
        let proto = encode_ack(
            &peer,
            SwarmNodeType::Client,
            &max_message,
            spec.network_id(),
        );
        assert!(decode_ack(proto, spec.network_id()).is_ok());

        let mut proto = encode_ack(&peer, SwarmNodeType::Client, "", spec.network_id());
        proto.welcome_message = "x".repeat(MAX_WELCOME_MESSAGE_CHARS + 1);
        assert!(matches!(
            decode_ack(proto, spec.network_id()),
            Err(HandshakeError::FieldTooLong { .. })
        ));
    }
}
