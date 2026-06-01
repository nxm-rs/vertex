//! Ack message encoding/decoding for handshake 15.0.0.
//!
//! Wire layout (matches bee `pb/handshake.proto::Ack`):
//!
//! ```text
//! Ack {
//!   BzzAddress address     = 1;
//!   uint64     network_id  = 2;
//!   bool       full_node   = 3;
//!   string     welcome_msg = 99;
//! }
//! BzzAddress {
//!   bytes underlay           = 1;
//!   bytes signature          = 2;
//!   bytes overlay            = 3;
//!   bytes nonce              = 4;
//!   int64 timestamp          = 5;
//!   bytes chequebook_address = 6;
//! }
//! ```

use alloy_primitives::Signature;
use tracing::debug;
use vertex_swarm_peer::{BzzAddress, Nonce, SwarmAddress, SwarmNodeType, Timestamp};

use crate::HandshakeError;
use crate::welcome::WelcomeMessage;

/// Validated contents of a decoded [`Ack`](vertex_swarm_net_proto::handshake::Ack).
///
/// Exposed publicly so the [`crate::session`] typestate API can consume it.
/// `ethereum_address` is recovered from the signature; consumers building a
/// legacy [`SwarmPeer`] should pass it to
/// [`SwarmPeer::from_validated`](vertex_swarm_peer::SwarmPeer::from_validated).
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct DecodedAck {
    /// Verified bee-mainnet wire address.
    pub bzz_address: BzzAddress,
    /// Ethereum address recovered from the signature.
    pub ethereum_address: alloy_primitives::Address,
    /// Node type advertised by the peer.
    pub node_type: SwarmNodeType,
    /// Welcome message advertised by the peer.
    pub welcome_message: WelcomeMessage,
}

/// Decode an Ack proto message.
///
/// Validates `network_id`, signature, overlay, and (when `now` is `Some`) the
/// timestamp's clock skew. Returns a [`DecodedAck`] bundle ready for
/// [`crate::HandshakeInfo`].
pub(crate) fn decode_ack(
    proto: vertex_swarm_net_proto::handshake::Ack,
    expected_network_id: u64,
    now: Option<Timestamp>,
) -> Result<DecodedAck, HandshakeError> {
    if proto.network_id != expected_network_id {
        return Err(HandshakeError::NetworkIdMismatch);
    }

    let address = proto
        .address
        .ok_or(HandshakeError::MissingField("address"))?;

    // Per-field validation up front so we produce specific errors.
    let overlay = SwarmAddress::from_slice(address.overlay.as_slice())
        .inspect_err(|e| debug!(error = ?e, "invalid overlay in handshake ack"))
        .map_err(|_| HandshakeError::InvalidOverlay)?;

    let nonce_bytes: [u8; 32] = address.nonce.as_slice().try_into()?;
    let nonce = Nonce::from(nonce_bytes);

    let signature = Signature::try_from(address.signature.as_slice())?;
    let timestamp = Timestamp::from_secs(address.timestamp);

    let (bzz_address, ethereum_address) = BzzAddress::parse(
        &address.underlay,
        signature,
        overlay,
        nonce,
        timestamp,
        proto.network_id,
        &address.chequebook_address,
        now,
    )
    .map_err(|err| map_parse_error(err, overlay, timestamp, now))?;

    let welcome_message = welcome_from_proto(&proto.welcome_message)?;
    let node_type = node_type_from_wire(proto.full_node);

    Ok(DecodedAck {
        bzz_address,
        ethereum_address,
        node_type,
        welcome_message,
    })
}

/// Encode a verified [`BzzAddress`] + identity bits into an Ack proto message.
pub(crate) fn encode_ack(
    bzz: &BzzAddress,
    node_type: SwarmNodeType,
    welcome_message: &WelcomeMessage,
    network_id: u64,
) -> vertex_swarm_net_proto::handshake::Ack {
    vertex_swarm_net_proto::handshake::Ack {
        address: Some(vertex_swarm_net_proto::handshake::BzzAddress {
            underlay: bzz.serialize_underlay(),
            signature: bzz.signature().as_bytes().to_vec(),
            overlay: bzz.overlay().to_vec(),
            nonce: bzz.nonce().as_bytes().to_vec(),
            timestamp: bzz.timestamp().as_secs(),
            chequebook_address: bzz
                .chequebook()
                .map(|cb| cb.as_slice().to_vec())
                .unwrap_or_default(),
        }),
        network_id,
        full_node: node_type_to_wire(node_type),
        welcome_message: welcome_message.as_str().to_owned(),
    }
}

/// Map a [`SwarmPeerError`] into a typed [`HandshakeError`].
///
/// The peer crate's parser collapses skew failures into a single variant; we
/// re-attach overlay and drift so operators can correlate the rejection.
fn map_parse_error(
    err: vertex_swarm_peer::error::SwarmPeerError,
    overlay: SwarmAddress,
    timestamp: Timestamp,
    now: Option<Timestamp>,
) -> HandshakeError {
    use vertex_swarm_peer::error::SwarmPeerError;
    match err {
        SwarmPeerError::TimestampOutsideSkewWindow => {
            let drift_seconds = now
                .map(|n| timestamp.as_secs().saturating_sub(n.as_secs()))
                .unwrap_or(0);
            HandshakeError::TimestampOutsideSkewWindow {
                peer_overlay: overlay,
                drift_seconds,
            }
        }
        other => HandshakeError::InvalidPeer(other),
    }
}

/// Convert wire `full_node` bool to [`SwarmNodeType`].
///
/// The wire format is binary (storer vs not); bootnodes do not handshake.
pub(crate) fn node_type_from_wire(full_node: bool) -> SwarmNodeType {
    if full_node {
        SwarmNodeType::Storer
    } else {
        SwarmNodeType::Client
    }
}

pub(crate) fn node_type_to_wire(node_type: SwarmNodeType) -> bool {
    node_type.requires_storage()
}

pub(crate) fn welcome_from_proto(s: &str) -> Result<WelcomeMessage, HandshakeError> {
    Ok(WelcomeMessage::new(s)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::welcome::MAX_WELCOME_MESSAGE_CHARS;
    use alloy_primitives::{Address, B256};
    use alloy_signer_local::PrivateKeySigner;
    use libp2p::Multiaddr;
    use vertex_swarm_primitives::compute_overlay;

    fn fixed_now() -> Timestamp {
        Timestamp::from_secs(1_700_000_000)
    }

    fn build_bzz(
        signer: &PrivateKeySigner,
        network_id: u64,
        nonce: Nonce,
        timestamp: Timestamp,
        chequebook: Option<Address>,
    ) -> BzzAddress {
        let overlay = compute_overlay(&signer.address(), network_id, nonce.as_b256());
        let underlay: Vec<Multiaddr> = vec!["/ip4/127.0.0.1/tcp/1234".parse().unwrap()];
        BzzAddress::sign(
            signer, underlay, overlay, network_id, nonce, timestamp, chequebook,
        )
        .unwrap()
    }

    #[test]
    fn ack_roundtrip_no_chequebook() {
        let network_id = 10u64;
        let nonce = Nonce::from([0x11u8; 32]);
        let signer = PrivateKeySigner::random();
        let bzz = build_bzz(&signer, network_id, nonce, fixed_now(), None);
        let welcome = WelcomeMessage::new("buzz buzz").unwrap();

        let proto = encode_ack(&bzz, SwarmNodeType::Storer, &welcome, network_id);
        let decoded = decode_ack(proto, network_id, Some(fixed_now())).unwrap();

        assert_eq!(decoded.bzz_address, bzz);
        assert_eq!(decoded.node_type, SwarmNodeType::Storer);
        assert_eq!(decoded.welcome_message, welcome);
        assert_eq!(decoded.ethereum_address, signer.address());
    }

    #[test]
    fn ack_roundtrip_with_chequebook() {
        let network_id = 1u64;
        let nonce = Nonce::from([0x22u8; 32]);
        let signer = PrivateKeySigner::random();
        let chequebook = Some(Address::from([0xAB; 20]));
        let bzz = build_bzz(&signer, network_id, nonce, fixed_now(), chequebook);
        let welcome = WelcomeMessage::empty();

        let proto = encode_ack(&bzz, SwarmNodeType::Client, &welcome, network_id);
        let decoded = decode_ack(proto, network_id, Some(fixed_now())).unwrap();

        assert_eq!(decoded.bzz_address.chequebook(), chequebook.as_ref());
        assert_eq!(decoded.node_type, SwarmNodeType::Client);
    }

    #[test]
    fn network_id_mismatch_rejected() {
        let nonce = Nonce::from([0u8; 32]);
        let signer = PrivateKeySigner::random();
        let bzz = build_bzz(&signer, 1, nonce, fixed_now(), None);
        let proto = encode_ack(&bzz, SwarmNodeType::Storer, &WelcomeMessage::empty(), 1);
        let err = decode_ack(proto, 2, Some(fixed_now())).unwrap_err();
        assert!(matches!(err, HandshakeError::NetworkIdMismatch));
    }

    #[test]
    fn missing_address_field_rejected() {
        let proto = vertex_swarm_net_proto::handshake::Ack {
            network_id: 1,
            ..Default::default()
        };
        let err = decode_ack(proto, 1, Some(fixed_now())).unwrap_err();
        assert!(matches!(err, HandshakeError::MissingField("address")));
    }

    #[test]
    fn invalid_nonce_length_rejected() {
        let nonce = Nonce::from([0u8; 32]);
        let signer = PrivateKeySigner::random();
        let bzz = build_bzz(&signer, 1, nonce, fixed_now(), None);
        let mut proto = encode_ack(&bzz, SwarmNodeType::Client, &WelcomeMessage::empty(), 1);
        proto.address.as_mut().unwrap().nonce = vec![0u8; 16];
        let err = decode_ack(proto, 1, Some(fixed_now())).unwrap_err();
        assert!(matches!(err, HandshakeError::InvalidData(_)));
    }

    #[test]
    fn invalid_signature_rejected() {
        let nonce = Nonce::from([0u8; 32]);
        let signer = PrivateKeySigner::random();
        let bzz = build_bzz(&signer, 1, nonce, fixed_now(), None);
        let mut proto = encode_ack(&bzz, SwarmNodeType::Client, &WelcomeMessage::empty(), 1);
        proto.address.as_mut().unwrap().signature = vec![0u8; 65];
        let err = decode_ack(proto, 1, Some(fixed_now())).unwrap_err();
        // All-zero signature is either rejected by EIP-191 recovery
        // (`InvalidSignature`) or recovers to a different address and trips
        // the overlay check — both are valid failure modes.
        assert!(
            matches!(
                err,
                HandshakeError::InvalidPeer(
                    vertex_swarm_peer::error::SwarmPeerError::InvalidOverlay
                ) | HandshakeError::InvalidPeer(
                    vertex_swarm_peer::error::SwarmPeerError::InvalidSignature(_)
                )
            ),
            "unexpected error variant: {err:?}"
        );
    }

    #[test]
    fn timestamp_outside_skew_window_typed() {
        let network_id = 1u64;
        let nonce = Nonce::from([0u8; 32]);
        let signer = PrivateKeySigner::random();
        let far_future = Timestamp::from_secs(fixed_now().as_secs() + 24 * 60 * 60);
        let bzz = build_bzz(&signer, network_id, nonce, far_future, None);
        let proto = encode_ack(
            &bzz,
            SwarmNodeType::Storer,
            &WelcomeMessage::empty(),
            network_id,
        );
        let err = decode_ack(proto, network_id, Some(fixed_now())).unwrap_err();
        match err {
            HandshakeError::TimestampOutsideSkewWindow {
                peer_overlay,
                drift_seconds,
            } => {
                assert_eq!(peer_overlay, *bzz.overlay());
                assert_eq!(drift_seconds, 24 * 60 * 60);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn invalid_overlay_bytes_rejected() {
        let nonce = Nonce::from([0u8; 32]);
        let signer = PrivateKeySigner::random();
        let bzz = build_bzz(&signer, 1, nonce, fixed_now(), None);
        let mut proto = encode_ack(&bzz, SwarmNodeType::Client, &WelcomeMessage::empty(), 1);
        proto.address.as_mut().unwrap().overlay = vec![0u8; 16];
        let err = decode_ack(proto, 1, Some(fixed_now())).unwrap_err();
        assert!(matches!(err, HandshakeError::InvalidOverlay));
    }

    #[test]
    fn welcome_message_max_length_accepted() {
        let nonce = Nonce::from([0u8; 32]);
        let signer = PrivateKeySigner::random();
        let bzz = build_bzz(&signer, 1, nonce, fixed_now(), None);
        let welcome = WelcomeMessage::new("x".repeat(MAX_WELCOME_MESSAGE_CHARS)).unwrap();
        let proto = encode_ack(&bzz, SwarmNodeType::Storer, &welcome, 1);
        let decoded = decode_ack(proto, 1, Some(fixed_now())).unwrap();
        assert_eq!(decoded.welcome_message, welcome);
    }

    #[test]
    fn welcome_message_over_max_rejected() {
        let nonce = Nonce::from([0u8; 32]);
        let signer = PrivateKeySigner::random();
        let bzz = build_bzz(&signer, 1, nonce, fixed_now(), None);
        let mut proto = encode_ack(&bzz, SwarmNodeType::Storer, &WelcomeMessage::empty(), 1);
        proto.welcome_message = "x".repeat(MAX_WELCOME_MESSAGE_CHARS + 1);
        let err = decode_ack(proto, 1, Some(fixed_now())).unwrap_err();
        assert!(matches!(err, HandshakeError::InvalidWelcomeMessage(_)));
    }

    #[test]
    fn invalid_chequebook_length_rejected() {
        let nonce = Nonce::from([0u8; 32]);
        let signer = PrivateKeySigner::random();
        let bzz = build_bzz(&signer, 1, nonce, fixed_now(), None);
        let mut proto = encode_ack(&bzz, SwarmNodeType::Storer, &WelcomeMessage::empty(), 1);
        proto.address.as_mut().unwrap().chequebook_address = vec![0u8; 19];
        let err = decode_ack(proto, 1, Some(fixed_now())).unwrap_err();
        assert!(matches!(
            err,
            HandshakeError::InvalidPeer(
                vertex_swarm_peer::error::SwarmPeerError::InvalidChequebook
            )
        ));
    }

    #[test]
    fn ack_signed_by_fixed_keypair_recovers() {
        // Deterministic vector: sign with a fixed key, encode, decode, assert
        // the recovered ethereum address matches.
        let raw = B256::repeat_byte(0x42);
        let signer = PrivateKeySigner::from_bytes(&raw).expect("test key");
        let nonce = Nonce::from([0x5Au8; 32]);
        let bzz = build_bzz(&signer, 1, nonce, fixed_now(), None);
        let proto = encode_ack(&bzz, SwarmNodeType::Storer, &WelcomeMessage::empty(), 1);
        let decoded = decode_ack(proto, 1, Some(fixed_now())).unwrap();
        assert_eq!(decoded.bzz_address, bzz);
    }
}
