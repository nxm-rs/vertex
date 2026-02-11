//! Ack message codec for handshake protocol.

use nectar_primitives::SwarmAddress;
use tracing::debug;
use vertex_net_codec::ProtoMessageWithContext;
use vertex_swarm_peer::{SwarmNodeType, SwarmPeer};

use crate::MAX_WELCOME_MESSAGE_CHARS;
use crate::HandshakeError;

/// Ack message containing peer identity and metadata.
#[derive(Clone)]
pub struct Ack {
    swarm_peer: SwarmPeer,
    node_type: SwarmNodeType,
    welcome_message: String,
}

impl Ack {
    /// Create a new Ack message.
    pub fn new(swarm_peer: SwarmPeer, node_type: SwarmNodeType, welcome_message: String) -> Self {
        Self {
            swarm_peer,
            node_type,
            welcome_message,
        }
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
    pub fn into_parts(self) -> (SwarmPeer, SwarmNodeType, String) {
        (self.swarm_peer, self.node_type, self.welcome_message)
    }
}

impl ProtoMessageWithContext<u64> for Ack {
    type Proto = crate::proto::handshake::Ack;
    type EncodeError = std::convert::Infallible;
    type DecodeError = HandshakeError;

    fn into_proto_with_context(self, network_id: &u64) -> Result<Self::Proto, Self::EncodeError> {
        Ok(swarm_peer_to_proto(
            &self.swarm_peer,
            *network_id,
            self.node_type,
            &self.welcome_message,
        ))
    }

    fn from_proto_with_context(
        proto: Self::Proto,
        expected_network_id: &u64,
    ) -> Result<Self, Self::DecodeError> {
        let swarm_peer = swarm_peer_from_proto(&proto, *expected_network_id)?;
        let welcome_message = welcome_message_from_proto(&proto)?;
        // Wire format uses bool for backwards compatibility.
        // TODO(#59): Change wire format to use node_type enum directly.
        let node_type = node_type_from_wire(proto.storer);
        Ok(Self::new(swarm_peer, node_type, welcome_message))
    }
}

/// Convert wire format bool to SwarmNodeType.
///
/// Wire format uses `bool storer` for backwards compatibility with existing peers.
/// `true` maps to `Storer`, `false` maps to `Client`.
///
/// Note: Bootnodes don't participate in handshake protocol (topology-only),
/// so we never receive Bootnode over the wire.
pub(crate) fn node_type_from_wire(storer: bool) -> SwarmNodeType {
    if storer {
        SwarmNodeType::Storer
    } else {
        SwarmNodeType::Client
    }
}

/// Convert SwarmNodeType to wire format bool.
fn node_type_to_wire(node_type: SwarmNodeType) -> bool {
    node_type.requires_storage()
}

/// Convert a proto::Ack to SwarmPeer, validating the network_id matches.
pub(crate) fn swarm_peer_from_proto(
    value: &crate::proto::handshake::Ack,
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

/// Validate and extract the welcome message from a proto::Ack.
pub(crate) fn welcome_message_from_proto(
    value: &crate::proto::handshake::Ack,
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

/// Encode a SwarmPeer and metadata into a proto::Ack for sending.
pub(crate) fn swarm_peer_to_proto(
    peer: &SwarmPeer,
    network_id: u64,
    node_type: SwarmNodeType,
    welcome_message: &str,
) -> crate::proto::handshake::Ack {
    crate::proto::handshake::Ack {
        peer: Some(crate::proto::handshake::PeerAddress {
            multiaddrs: peer.serialize_multiaddrs(),
            signature: peer.signature().as_bytes().to_vec(),
            overlay: peer.overlay().to_vec(),
        }),
        network_id,
        // Wire format uses bool for backwards compatibility
        storer: node_type_to_wire(node_type),
        nonce: peer.nonce().to_vec(),
        welcome_message: welcome_message.to_string(),
    }
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
        let ack = Ack::new(peer.clone(), SwarmNodeType::Storer, "hello".to_string());

        let proto = ack.clone().into_proto_with_context(&spec.network_id()).unwrap();
        let recovered = Ack::from_proto_with_context(proto, &spec.network_id()).unwrap();

        assert_eq!(ack.swarm_peer(), recovered.swarm_peer());
        assert_eq!(ack.node_type(), recovered.node_type());
        assert_eq!(ack.welcome_message(), recovered.welcome_message());
    }

    #[test]
    fn test_proto_roundtrip() {
        let spec = test_spec();
        let peer = create_test_peer();
        let node_type = SwarmNodeType::Storer;
        let welcome_message = "hello";

        let proto_ack = swarm_peer_to_proto(&peer, spec.network_id(), node_type, welcome_message);

        let recovered_peer = swarm_peer_from_proto(&proto_ack, spec.network_id()).unwrap();
        let recovered_message = welcome_message_from_proto(&proto_ack).unwrap();

        assert_eq!(peer, recovered_peer);
        // Wire format uses bool
        assert!(proto_ack.storer); // Storer -> true
        assert_eq!(recovered_message, welcome_message);
    }

    #[test]
    fn test_wire_format_backwards_compat() {
        // Test that bool wire format converts correctly
        assert_eq!(node_type_from_wire(true), SwarmNodeType::Storer);
        assert_eq!(node_type_from_wire(false), SwarmNodeType::Client);
        assert!(node_type_to_wire(SwarmNodeType::Storer));
        assert!(!node_type_to_wire(SwarmNodeType::Client));
        assert!(!node_type_to_wire(SwarmNodeType::Bootnode));
    }

    #[test]
    fn test_empty_multiaddrs_rejected() {
        let spec = test_spec();
        let peer = create_test_peer();
        let mut proto_ack = swarm_peer_to_proto(&peer, spec.network_id(), SwarmNodeType::Client, "test");

        if let Some(ref mut peer_addr) = proto_ack.peer {
            peer_addr.multiaddrs = vec![];
        }

        let result = swarm_peer_from_proto(&proto_ack, spec.network_id());
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
    fn test_welcome_message_validation() {
        let spec = test_spec();
        let peer = create_test_peer();
        let base_char = "x";

        // Test with message at max length - should succeed
        let max_message = base_char.repeat(MAX_WELCOME_MESSAGE_CHARS);
        let proto_ack = swarm_peer_to_proto(&peer, spec.network_id(), SwarmNodeType::Client, &max_message);
        assert!(welcome_message_from_proto(&proto_ack).is_ok());

        // Test with message exceeding max length - should fail
        let mut proto_ack = swarm_peer_to_proto(&peer, spec.network_id(), SwarmNodeType::Client, "");
        proto_ack.welcome_message = base_char.repeat(MAX_WELCOME_MESSAGE_CHARS + 1);
        assert!(matches!(
            welcome_message_from_proto(&proto_ack),
            Err(HandshakeError::FieldTooLong { .. })
        ));
    }

    #[test]
    fn test_network_id_validation() {
        let spec = test_spec();
        let peer = create_test_peer();
        let proto_ack = swarm_peer_to_proto(&peer, spec.network_id(), SwarmNodeType::Client, "hello");

        let wrong_network_id = spec.network_id().wrapping_add(1);
        let result = swarm_peer_from_proto(&proto_ack, wrong_network_id);
        assert!(matches!(result, Err(HandshakeError::NetworkIdMismatch)));
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn test_malformed_proto() {
        let spec = test_spec();
        let peer = create_test_peer();
        let proto_ack = swarm_peer_to_proto(&peer, spec.network_id(), SwarmNodeType::Client, "test");

        type AckModifier =
            Box<dyn Fn(crate::proto::handshake::Ack) -> crate::proto::handshake::Ack>;

        let test_cases: Vec<(AckModifier, Box<dyn Fn(&HandshakeError) -> bool>)> = vec![
            (
                Box::new(|mut ack| {
                    ack.peer = None;
                    ack
                }),
                Box::new(|e| matches!(e, HandshakeError::MissingField("peer"))),
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
                        HandshakeError::InvalidPeer(
                            vertex_swarm_peer::SwarmPeerError::InvalidSignature(_)
                        )
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
                        HandshakeError::InvalidPeer(
                            vertex_swarm_peer::SwarmPeerError::InvalidMultiaddr(_)
                        )
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
                        HandshakeError::InvalidPeer(
                            vertex_swarm_peer::SwarmPeerError::InvalidOverlay
                        )
                    )
                }),
            ),
            (
                Box::new(|mut ack| {
                    ack.nonce = vec![0u8; 16];
                    ack
                }),
                Box::new(|e| matches!(e, HandshakeError::InvalidData(_))),
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
