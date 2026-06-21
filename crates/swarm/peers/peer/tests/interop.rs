//! Wire-conformance vectors for the Swarm handshake 15.0.0 [`SwarmPeer`].
//!
//! These vectors pin the on-the-wire byte layout that every conforming
//! Swarm implementation must produce for the same inputs. Two layers are
//! pinned:
//!
//! 1. Overlay derivation: `keccak256(eth_address || network_id_le(8) || nonce(32))`.
//! 2. Handshake 15.0.0 sign-data and the EIP-191 personal-sign signature
//!    over it: `"bee-handshake-" || multiaddrs || overlay || network_id_be(8)
//!    || nonce(32) || timestamp_be(8) || chequebook(20)`.
//!
//! Vertex is exercised through the public [`SwarmPeer::sign`] and
//! [`SwarmPeer::parse`] API, not a private reimplementation of the byte
//! layout; the assertions hold exactly when vertex's wire bytes are
//! identical to any other conforming implementation's. RFC 6979
//! secp256k1 ECDSA pins the signature deterministically, so a single
//! byte of drift in either the sign-data layout or the overlay
//! derivation surfaces as a vector mismatch.
//!
//! The overlay-only table is reproduced from the published bee
//! reference vectors, which are themselves the canonical Swarm spec
//! values for `compute_overlay`.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "conformance fixtures: panicking on malformed test inputs is intended"
)]

use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::{Address, B256, Signature};
use alloy_signer_local::PrivateKeySigner;
use libp2p::Multiaddr;
use nectar_primitives::SwarmAddress;
use vertex_swarm_identity::Identity;
use vertex_swarm_peer::{SwarmPeer, SwarmPeerWire};
use vertex_swarm_primitives::{NetworkId, Nonce, SwarmNodeType, Timestamp, compute_overlay};
use vertex_swarm_spec::SpecBuilder;

/// A persistent identity reproducing the vector's signer, nonce and network id,
/// so `SwarmPeer::sign` derives exactly the vector's pinned overlay.
fn vector_identity(signer: PrivateKeySigner, network_id: NetworkId, nonce: Nonce) -> Identity {
    let spec = Arc::new(SpecBuilder::testnet().network_id(network_id.get()).build());
    Identity::new(signer, nonce, spec, SwarmNodeType::Storer)
}

/// Decode a fixed-size lowercase hex string into a byte array.
fn hex_to_array<const N: usize>(hex: &str) -> [u8; N] {
    let trimmed = hex.strip_prefix("0x").unwrap_or(hex);
    assert_eq!(
        trimmed.len(),
        N * 2,
        "hex literal `{hex}` has length {} but expected {}",
        trimmed.len(),
        N * 2,
    );
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        let off = i * 2;
        *byte = u8::from_str_radix(&trimmed[off..off + 2], 16)
            .expect("hex literal contains non-hex characters");
    }
    out
}

/// Overlay-only conformance vectors. Every Swarm node must produce the
/// listed overlay for the corresponding `(ethereum_address, network_id,
/// nonce)` triple.
const OVERLAY_VECTORS: &[OverlayVector] = &[
    OverlayVector {
        eth_address_hex: "1815cac638d1525b47f848daf02b7953e4edd15c",
        network_id: 1,
        nonce_hex: "0000000000000000000000000000000000000000000000000000000000000001",
        expected_overlay_hex: "a38f7a814d4b249ae9d3821e9b898019c78ac9abe248fff171782c32a3849a17",
    },
    OverlayVector {
        eth_address_hex: "1815cac638d1525b47f848daf02b7953e4edd15c",
        network_id: 1,
        nonce_hex: "0000000000000000000000000000000000000000000000000000000000000002",
        expected_overlay_hex: "c63c10b1728dfc463c64c264f71a621fe640196979375840be42dc496b702610",
    },
    OverlayVector {
        eth_address_hex: "d26bc1715e933bd5f8fad16310042f13abc16159",
        network_id: 2,
        nonce_hex: "0000000000000000000000000000000000000000000000000000000000000001",
        expected_overlay_hex: "9f421f9149b8e31e238cfbdc6e5e833bacf1e42f77f60874d49291292858968e",
    },
    OverlayVector {
        eth_address_hex: "ac485e3c63dcf9b4cda9f007628bb0b6fed1c063",
        network_id: 1,
        nonce_hex: "0000000000000000000000000000000000000000000000000000000000000000",
        expected_overlay_hex: "fe3a6d582c577404fb19df64a44e00d3a3b71230a8464c0dd34af3f0791b45f2",
    },
];

struct OverlayVector {
    eth_address_hex: &'static str,
    network_id: u64,
    nonce_hex: &'static str,
    expected_overlay_hex: &'static str,
}

#[test]
fn overlay_derivation_matches_swarm_spec() {
    for (idx, v) in OVERLAY_VECTORS.iter().enumerate() {
        let eth = Address::from(hex_to_array::<20>(v.eth_address_hex));
        let nonce = Nonce::from(hex_to_array::<32>(v.nonce_hex));
        let expected = SwarmAddress::from(hex_to_array::<32>(v.expected_overlay_hex));

        let got = compute_overlay(&eth, NetworkId::new(v.network_id), &nonce);
        assert_eq!(got, expected, "overlay vector {idx} mismatch");
    }
}

/// Full handshake 15.0.0 conformance vector: the inputs uniquely
/// determine the overlay (via [`compute_overlay`]) and the signature
/// (via RFC 6979 ECDSA over the canonical sign-data layout). Pinning
/// both expected values surfaces any drift on either side.
struct HandshakeVector {
    /// 32-byte secp256k1 private key.
    private_key_hex: &'static str,
    /// Swarm network id (little-endian in overlay, big-endian in sign-data).
    network_id: u64,
    /// 32-byte handshake nonce.
    nonce_hex: &'static str,
    /// Signed wall-clock seconds.
    timestamp: i64,
    /// Single multiaddr (parsed via [`Multiaddr::from_str`]).
    multiaddr: &'static str,
    /// Optional 20-byte chequebook address; `None` pads to 20 zero bytes
    /// in sign-data.
    chequebook_hex: Option<&'static str>,
    /// Ethereum address derived from `private_key_hex`.
    expected_eth_address_hex: &'static str,
    /// Overlay produced by [`compute_overlay`].
    expected_overlay_hex: &'static str,
    /// EIP-191 ECDSA signature over sign-data (`r || s || v`, 65 bytes;
    /// `v` is the raw recovery byte `0x1b` or `0x1c`).
    expected_signature_hex: &'static str,
}

const HANDSHAKE_VECTORS: &[HandshakeVector] = &[
    HandshakeVector {
        private_key_hex: "2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae",
        network_id: 10,
        nonce_hex: "0000000000000000000000000000000000000000000000000000000000000002",
        timestamp: 1_700_000_000,
        multiaddr: "/ip4/127.0.0.1/tcp/1634",
        chequebook_hex: None,
        expected_eth_address_hex: "2f63cbeb054ce76050827e42dd75268f6b9d87c5",
        expected_overlay_hex: "56c67d004cbcb0de9b5020bb37d0efb5f6c8f568049e6e9bfa0fc0ae696f0509",
        expected_signature_hex: "3d916e1bc20f622bc275275f8e76cc8ee255ca513745cf2982b2e0c17be267015ec7472c79fd40ac15a115ba71ea353dba3c507d3c5b83eb53f0e9bad9b44bfa1c",
    },
    HandshakeVector {
        private_key_hex: "2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae",
        network_id: 1,
        nonce_hex: "0000000000000000000000000000000000000000000000000000000000000001",
        timestamp: 1_700_000_000,
        multiaddr: "/ip4/127.0.0.1/tcp/1634",
        chequebook_hex: Some("abc0000000000000000000000000000000000123"),
        expected_eth_address_hex: "2f63cbeb054ce76050827e42dd75268f6b9d87c5",
        expected_overlay_hex: "5c447a6f6e2e8c875fc744295e71308a9d406d1a3f093cf3c7d94c8f7acbb836",
        expected_signature_hex: "d4728d596c63ff24ca0a4599912356d186bd0b91cd17b0918caf0f064b05b44c7b2c7242a4c4db9dd447cfce90d2012c47e001371eddc7c7746df39265b3f21e1c",
    },
];

impl HandshakeVector {
    fn signer(&self) -> PrivateKeySigner {
        let bytes = hex_to_array::<32>(self.private_key_hex);
        PrivateKeySigner::from_bytes(&B256::from(bytes)).expect("valid secp256k1 key")
    }

    fn nonce(&self) -> Nonce {
        Nonce::from(hex_to_array::<32>(self.nonce_hex))
    }

    fn multiaddr(&self) -> Multiaddr {
        self.multiaddr.parse().expect("valid multiaddr literal")
    }

    fn chequebook(&self) -> Option<Address> {
        self.chequebook_hex
            .map(|hex| Address::from(hex_to_array::<20>(hex)))
    }

    fn expected_eth_address(&self) -> Address {
        Address::from(hex_to_array::<20>(self.expected_eth_address_hex))
    }

    fn expected_overlay(&self) -> SwarmAddress {
        SwarmAddress::from(hex_to_array::<32>(self.expected_overlay_hex))
    }

    fn expected_signature(&self) -> Signature {
        let bytes = hex_to_array::<65>(self.expected_signature_hex);
        Signature::from_raw(&bytes).expect("valid signature literal")
    }
}

#[test]
fn handshake_15_sign_produces_pinned_signature_and_overlay() {
    for (idx, v) in HANDSHAKE_VECTORS.iter().enumerate() {
        let signer = v.signer();
        let network_id = NetworkId::new(v.network_id);
        let nonce = v.nonce();
        let timestamp = Timestamp::from_seconds(v.timestamp);
        let multiaddr = v.multiaddr();
        let chequebook = v.chequebook();

        assert_eq!(
            signer.address(),
            v.expected_eth_address(),
            "vector {idx}: derived ethereum address mismatch",
        );

        let overlay = compute_overlay(&signer.address(), network_id, &nonce);
        assert_eq!(
            overlay,
            v.expected_overlay(),
            "vector {idx}: overlay mismatch",
        );

        let identity = vector_identity(signer, network_id, nonce);
        let peer = SwarmPeer::sign(&identity, vec![multiaddr], timestamp, chequebook)
            .expect("sign succeeds");

        assert_eq!(
            *peer.signature(),
            v.expected_signature(),
            "vector {idx}: signature does not match pinned conformance value; \
             sign-data layout has drifted",
        );
    }
}

#[test]
fn handshake_15_parse_recovers_pinned_signer() {
    for (idx, v) in HANDSHAKE_VECTORS.iter().enumerate() {
        let signer = v.signer();
        let network_id = NetworkId::new(v.network_id);
        let nonce = v.nonce();
        let timestamp = Timestamp::from_seconds(v.timestamp);
        let multiaddr = v.multiaddr();
        let chequebook = v.chequebook();

        let identity = vector_identity(signer, network_id, nonce);
        let peer = SwarmPeer::sign(&identity, vec![multiaddr], timestamp, chequebook)
            .expect("sign succeeds");

        let multiaddrs_bytes = peer.serialize_multiaddrs();
        let chequebook_bytes = peer
            .chequebook()
            .map_or_else(Vec::new, |addr| addr.as_slice().to_vec());

        let wire = SwarmPeerWire {
            multiaddrs_bytes: &multiaddrs_bytes,
            signature: *peer.signature(),
            overlay: *peer.overlay(),
            nonce: *peer.nonce(),
            timestamp: peer.timestamp(),
            chequebook_bytes: &chequebook_bytes,
        };

        let parsed = SwarmPeer::parse(
            wire,
            network_id,
            Some((
                Timestamp::from_seconds(v.timestamp),
                Duration::from_secs(60),
            )),
        )
        .unwrap_or_else(|err| panic!("vector {idx}: parse failed: {err}"));

        assert_eq!(
            *parsed.ethereum_address(),
            v.expected_eth_address(),
            "vector {idx}: recovered ethereum address mismatch",
        );
        assert_eq!(
            parsed.overlay(),
            peer.overlay(),
            "vector {idx}: overlay roundtrip mismatch",
        );
        assert_eq!(
            parsed.chequebook(),
            v.chequebook().as_ref(),
            "vector {idx}: chequebook roundtrip mismatch",
        );
    }
}
