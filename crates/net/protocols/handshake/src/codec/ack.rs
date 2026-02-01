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
    use std::sync::Arc;

    use super::*;
    use alloy_primitives::B256;
    use alloy_signer::k256::ecdsa::SigningKey;
    use alloy_signer_local::{LocalSigner, PrivateKeySigner};
    use libp2p::Multiaddr;
    use vertex_net_codec::ProtocolCodecError;

    const TEST_NETWORK_ID: u64 = 1234567890;

    fn create_test_peer(network_id: u64, signer: Arc<LocalSigner<SigningKey>>) -> SwarmPeer {
        let multiaddr: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();
        SwarmPeer::with_signer(vec![multiaddr], B256::default(), network_id, signer)
            .expect("should create peer")
    }

    #[test]
    fn test_ack_roundtrip() {
        let signer = Arc::new(PrivateKeySigner::random());
        let peer = create_test_peer(TEST_NETWORK_ID, signer);
        let ack = Ack::new(peer.clone(), true, "hello".to_string());

        // Convert to proto with context and back
        let proto = ack.clone().into_proto_with_context(&TEST_NETWORK_ID);
        let recovered = Ack::from_proto_with_context(proto, &TEST_NETWORK_ID).unwrap();

        assert_eq!(ack.swarm_peer(), recovered.swarm_peer());
        assert_eq!(ack.full_node(), recovered.full_node());
        assert_eq!(ack.welcome_message(), recovered.welcome_message());
    }

    #[test]
    fn test_proto_roundtrip() {
        let signer = Arc::new(PrivateKeySigner::random());
        let peer = create_test_peer(TEST_NETWORK_ID, signer);
        let full_node = true;
        let welcome_message = "hello";

        // Encode to proto
        let proto_ack = swarm_peer_to_proto(&peer, TEST_NETWORK_ID, full_node, welcome_message);

        // Decode back
        let recovered_peer = swarm_peer_from_proto(&proto_ack, TEST_NETWORK_ID).unwrap();
        let recovered_message = welcome_message_from_proto(&proto_ack).unwrap();

        // Verify
        assert_eq!(peer, recovered_peer);
        assert_eq!(proto_ack.full_node, full_node);
        assert_eq!(recovered_message, welcome_message);
    }

    #[test]
    fn test_inbound_only_peer() {
        let signer = Arc::new(PrivateKeySigner::random());
        let peer = create_test_peer(TEST_NETWORK_ID, signer);
        let mut proto_ack = swarm_peer_to_proto(&peer, TEST_NETWORK_ID, false, "browser peer");

        // Clear multiaddrs to simulate an inbound-only peer
        if let Some(ref mut peer_addr) = proto_ack.peer {
            peer_addr.multiaddrs = vec![];
        }

        // The signature won't be valid because it was signed with the original multiaddrs
        let result = swarm_peer_from_proto(&proto_ack, TEST_NETWORK_ID);
        assert!(
            matches!(
                result,
                Err(ProtocolCodecError::Domain(
                    HandshakeCodecDomainError::InvalidPeer(
                        vertex_swarm_peer::SwarmPeerError::InvalidSignature(_)
                            | vertex_swarm_peer::SwarmPeerError::InvalidOverlay
                    )
                ))
            ),
            "expected signature/overlay error, not multiaddr error"
        );
    }

    #[test]
    fn test_welcome_message_validation() {
        let signer = Arc::new(PrivateKeySigner::random());
        let peer = create_test_peer(TEST_NETWORK_ID, signer);
        let base_char = "x";

        // Test with message at max length - should succeed
        let max_message = base_char.repeat(MAX_WELCOME_MESSAGE_CHARS);
        let proto_ack = swarm_peer_to_proto(&peer, TEST_NETWORK_ID, false, &max_message);
        assert!(welcome_message_from_proto(&proto_ack).is_ok());

        // Test with message exceeding max length - should fail
        let mut proto_ack = swarm_peer_to_proto(&peer, TEST_NETWORK_ID, false, "");
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
        let signer = Arc::new(PrivateKeySigner::random());
        let peer = create_test_peer(TEST_NETWORK_ID, signer);
        let proto_ack = swarm_peer_to_proto(&peer, TEST_NETWORK_ID, false, "hello");

        let wrong_network_id = TEST_NETWORK_ID.wrapping_add(1);
        let result = swarm_peer_from_proto(&proto_ack, wrong_network_id);
        assert!(matches!(
            result,
            Err(ProtocolCodecError::Domain(
                HandshakeCodecDomainError::NetworkIdMismatch
            ))
        ));
    }

    #[test]
    fn test_malformed_proto() {
        let signer = Arc::new(PrivateKeySigner::random());
        let peer = create_test_peer(TEST_NETWORK_ID, signer);
        let proto_ack = swarm_peer_to_proto(&peer, TEST_NETWORK_ID, false, "test");

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
                // Now handled inside SwarmPeer::from_signed, so error is InvalidPeer(InvalidMultiaddr)
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
            match swarm_peer_from_proto(&modified_ack, TEST_NETWORK_ID) {
                Err(ref e) => assert!(check_error(e)),
                Ok(_) => panic!("expected error"),
            }
        }
    }
}
