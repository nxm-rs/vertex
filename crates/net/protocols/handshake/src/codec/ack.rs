//! Ack message codec for handshake protocol.
//!
//! This module handles encoding/decoding of ACK messages, converting between
//! the wire format (`proto::Ack`) and the domain type (`Ack`).

use nectar_primitives::SwarmAddress;
use vertex_net_codec::ProtoMessageWithContext;
use vertex_swarm_peer::SwarmPeer;

use crate::MAX_WELCOME_MESSAGE_CHARS;
use crate::codec::error::{CodecError, HandshakeCodecDomainError};

/// Ack message containing peer identity and metadata.
///
/// This is sent as the final message in the handshake to confirm identity.
#[derive(Clone)]
pub struct Ack {
    swarm_peer: SwarmPeer,
    full_node: bool,
    welcome_message: String,
}

impl Ack {
    /// Create a new Ack message.
    pub fn new(swarm_peer: SwarmPeer, full_node: bool, welcome_message: String) -> Self {
        Self {
            swarm_peer,
            full_node,
            welcome_message,
        }
    }

    pub fn swarm_peer(&self) -> &SwarmPeer {
        &self.swarm_peer
    }

    pub fn full_node(&self) -> bool {
        self.full_node
    }

    pub fn welcome_message(&self) -> &str {
        &self.welcome_message
    }

    /// Consume and return the components.
    pub fn into_parts(self) -> (SwarmPeer, bool, String) {
        (self.swarm_peer, self.full_node, self.welcome_message)
    }
}

impl ProtoMessageWithContext<u64> for Ack {
    type Proto = crate::proto::handshake::Ack;
    type DecodeError = CodecError;

    fn into_proto_with_context(self, network_id: &u64) -> Self::Proto {
        swarm_peer_to_proto(
            &self.swarm_peer,
            *network_id,
            self.full_node,
            &self.welcome_message,
        )
    }

    fn from_proto_with_context(
        proto: Self::Proto,
        expected_network_id: &u64,
    ) -> Result<Self, Self::DecodeError> {
        let swarm_peer = swarm_peer_from_proto(&proto, *expected_network_id)?;
        let welcome_message = welcome_message_from_proto(&proto)?;
        Ok(Self::new(swarm_peer, proto.full_node, welcome_message))
    }
}

/// Convert a proto::Ack to SwarmPeer, validating the network_id matches.
///
/// # Inbound-Only Peers
///
/// Empty multiaddrs are allowed for inbound-only peers (browsers, WebRTC, NAT-restricted)
/// that cannot be dialed back. These peers can participate in protocols but will not
/// be added to Kademlia topology or hive gossip.
pub(crate) fn swarm_peer_from_proto(
    value: &crate::proto::handshake::Ack,
    expected_network_id: u64,
) -> Result<SwarmPeer, CodecError> {
    if value.network_id != expected_network_id {
        return Err(CodecError::domain(
            HandshakeCodecDomainError::NetworkIdMismatch,
        ));
    }

    let peer_address = value
        .peer
        .as_ref()
        .ok_or_else(|| CodecError::domain(HandshakeCodecDomainError::MissingField("peer")))?;

    let overlay = SwarmAddress::from_slice(peer_address.overlay.as_slice())
        .map_err(|_| CodecError::protocol("invalid overlay"))?;

    let nonce = value
        .nonce
        .as_slice()
        .try_into()
        .map_err(HandshakeCodecDomainError::from)
        .map_err(CodecError::domain)?;
    let signature = peer_address
        .signature
        .as_slice()
        .try_into()
        .map_err(HandshakeCodecDomainError::from)
        .map_err(CodecError::domain)?;

    // Create SwarmPeer from raw multiaddr bytes (used for signature verification)
    SwarmPeer::from_signed(
        &peer_address.multiaddrs,
        signature,
        overlay,
        nonce,
        value.network_id,
        // Validate overlay derivation at the codec level
        true,
    )
    .map_err(HandshakeCodecDomainError::from)
    .map_err(CodecError::domain)
}

/// Validate and extract the welcome message from a proto::Ack.
pub(crate) fn welcome_message_from_proto(
    value: &crate::proto::handshake::Ack,
) -> Result<String, CodecError> {
    let char_count = value.welcome_message.chars().count();
    if char_count > MAX_WELCOME_MESSAGE_CHARS {
        return Err(CodecError::domain(
            HandshakeCodecDomainError::FieldLengthExceeded(
                "welcome_message",
                MAX_WELCOME_MESSAGE_CHARS,
                char_count,
            ),
        ));
    }
    Ok(value.welcome_message.clone())
}

/// Encode a SwarmPeer and metadata into a proto::Ack for sending.
pub(crate) fn swarm_peer_to_proto(
    peer: &SwarmPeer,
    network_id: u64,
    full_node: bool,
    welcome_message: &str,
) -> crate::proto::handshake::Ack {
    crate::proto::handshake::Ack {
        peer: Some(crate::proto::handshake::PeerAddress {
            multiaddrs: peer.serialize_multiaddrs(),
            signature: peer.signature().as_bytes().to_vec(),
            overlay: peer.overlay().to_vec(),
        }),
        network_id,
        full_node,
        nonce: peer.nonce().to_vec(),
        welcome_message: welcome_message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::Multiaddr;
    use vertex_net_codec::ProtocolCodecError;
    use vertex_swarm_identity::Identity;
    use vertex_swarm_peer::SwarmNodeType;
    use vertex_swarm_spec::{SpecBuilder, SwarmSpec};

    fn test_spec() -> vertex_swarm_spec::Spec {
        SpecBuilder::testnet().network_id(1234567890).build()
    }

    fn create_test_peer() -> SwarmPeer {
        let spec = test_spec();
        let identity = Identity::random(spec, SwarmNodeType::Storer);
        let multiaddr: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();
        SwarmPeer::from_identity(&identity, vec![multiaddr]).expect("should create peer")
    }

    #[test]
    fn test_ack_roundtrip() {
        let spec = test_spec();
        let peer = create_test_peer();
        let ack = Ack::new(peer.clone(), true, "hello".to_string());

        let proto = ack.clone().into_proto_with_context(&spec.network_id());
        let recovered = Ack::from_proto_with_context(proto, &spec.network_id()).unwrap();

        assert_eq!(ack.swarm_peer(), recovered.swarm_peer());
        assert_eq!(ack.full_node(), recovered.full_node());
        assert_eq!(ack.welcome_message(), recovered.welcome_message());
    }

    #[test]
    fn test_proto_roundtrip() {
        let spec = test_spec();
        let peer = create_test_peer();
        let full_node = true;
        let welcome_message = "hello";

        let proto_ack = swarm_peer_to_proto(&peer, spec.network_id(), full_node, welcome_message);

        let recovered_peer = swarm_peer_from_proto(&proto_ack, spec.network_id()).unwrap();
        let recovered_message = welcome_message_from_proto(&proto_ack).unwrap();

        assert_eq!(peer, recovered_peer);
        assert_eq!(proto_ack.full_node, full_node);
        assert_eq!(recovered_message, welcome_message);
    }

    #[test]
    fn test_empty_multiaddrs_rejected() {
        let spec = test_spec();
        let peer = create_test_peer();
        let mut proto_ack = swarm_peer_to_proto(&peer, spec.network_id(), false, "test");

        // Clear multiaddrs - should be rejected
        if let Some(ref mut peer_addr) = proto_ack.peer {
            peer_addr.multiaddrs = vec![];
        }

        let result = swarm_peer_from_proto(&proto_ack, spec.network_id());
        assert!(
            matches!(
                result,
                Err(ProtocolCodecError::Domain(
                    HandshakeCodecDomainError::InvalidPeer(
                        vertex_swarm_peer::SwarmPeerError::NoMultiaddrs
                    )
                ))
            ),
            "expected NoMultiaddrs error, got: {:?}",
            result
        );
    }

    #[test]
    fn test_welcome_message_validation() {
        let spec = test_spec();
        let peer = create_test_peer();
        let base_char = "x";

        // Test with message at max length - should succeed
        let max_message = base_char.repeat(MAX_WELCOME_MESSAGE_CHARS);
        let proto_ack = swarm_peer_to_proto(&peer, spec.network_id(), false, &max_message);
        assert!(welcome_message_from_proto(&proto_ack).is_ok());

        // Test with message exceeding max length - should fail
        let mut proto_ack = swarm_peer_to_proto(&peer, spec.network_id(), false, "");
        proto_ack.welcome_message = base_char.repeat(MAX_WELCOME_MESSAGE_CHARS + 1);
        assert!(matches!(
            welcome_message_from_proto(&proto_ack),
            Err(ProtocolCodecError::Domain(
                HandshakeCodecDomainError::FieldLengthExceeded(_, _, _)
            ))
        ));
    }

    #[test]
    fn test_network_id_validation() {
        let spec = test_spec();
        let peer = create_test_peer();
        let proto_ack = swarm_peer_to_proto(&peer, spec.network_id(), false, "hello");

        let wrong_network_id = spec.network_id().wrapping_add(1);
        let result = swarm_peer_from_proto(&proto_ack, wrong_network_id);
        assert!(matches!(
            result,
            Err(ProtocolCodecError::Domain(
                HandshakeCodecDomainError::NetworkIdMismatch
            ))
        ));
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn test_malformed_proto() {
        let spec = test_spec();
        let peer = create_test_peer();
        let proto_ack = swarm_peer_to_proto(&peer, spec.network_id(), false, "test");

        type AckModifier =
            Box<dyn Fn(crate::proto::handshake::Ack) -> crate::proto::handshake::Ack>;

        let test_cases: Vec<(AckModifier, Box<dyn Fn(&CodecError) -> bool>)> = vec![
            (
                Box::new(|mut ack| {
                    ack.peer = None;
                    ack
                }),
                Box::new(|e| {
                    matches!(
                        e,
                        ProtocolCodecError::Domain(HandshakeCodecDomainError::MissingField("peer"))
                    )
                }),
            ),
            (
                Box::new(|mut ack| {
                    let mut peer = ack.peer.unwrap();
                    peer.signature = vec![0u8; 65];
                    ack.peer = Some(peer);
                    ack
                }),
                Box::new(|e| {
                    matches!(
                        e,
                        ProtocolCodecError::Domain(HandshakeCodecDomainError::InvalidPeer(
                            vertex_swarm_peer::SwarmPeerError::InvalidSignature(_)
                        ))
                    )
                }),
            ),
            (
                Box::new(|mut ack| {
                    let mut peer = ack.peer.unwrap();
                    peer.multiaddrs = vec![0u8; 32];
                    ack.peer = Some(peer);
                    ack
                }),
                Box::new(|e| {
                    matches!(
                        e,
                        ProtocolCodecError::Domain(HandshakeCodecDomainError::InvalidPeer(
                            vertex_swarm_peer::SwarmPeerError::InvalidMultiaddr(_)
                        ))
                    )
                }),
            ),
            (
                Box::new(|mut ack| {
                    let mut peer = ack.peer.unwrap();
                    peer.overlay = vec![0u8; 32];
                    ack.peer = Some(peer);
                    ack
                }),
                Box::new(|e| {
                    matches!(
                        e,
                        ProtocolCodecError::Domain(HandshakeCodecDomainError::InvalidPeer(
                            vertex_swarm_peer::SwarmPeerError::InvalidOverlay
                        ))
                    )
                }),
            ),
            (
                Box::new(|mut ack| {
                    ack.nonce = vec![0u8; 16];
                    ack
                }),
                Box::new(|e| {
                    matches!(
                        e,
                        ProtocolCodecError::Domain(HandshakeCodecDomainError::InvalidData(_))
                    )
                }),
            ),
        ];

        for (modify_ack, check_error) in test_cases {
            let modified_ack = modify_ack(proto_ack.clone());
            match swarm_peer_from_proto(&modified_ack, spec.network_id()) {
                Err(ref e) => assert!(check_error(e)),
                Ok(_) => panic!("expected error"),
            }
        }
    }
}
