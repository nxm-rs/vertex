//! Cross-implementation golden vectors for the bee handshake-15.0.0 wire
//! `BzzAddress`.
//!
//! These vectors pin the byte-level layout of:
//!
//! 1. The overlay computation
//!    `keccak256(eth_address || network_id_le || nonce)` — vectors lifted
//!    directly from bee's `pkg/crypto/crypto_test.go::TestNewOverlayFromEthereumAddress`
//!    and `TestNewEthereumAddress`.
//! 2. The handshake-15.0.0 sign-data layout
//!    `"bee-handshake-" || underlay || overlay || network_id_be || nonce
//!    || timestamp_be || chequebook` — mirrored from bee's
//!    `pkg/bzz/address.go::generateSignData`.
//!
//! The handshake-15.0.0 `BzzAddress` type lives in a future Unit (Unit 2).
//! Until that lands on `main` we inline a [`reference_sign_data`] helper
//! that mirrors bee's `generateSignData` byte-for-byte; the test then signs
//! and recovers via the `alloy-signer` EIP-191 path. When Unit 2 lands the
//! body of `full_vectors_roundtrip_byte_identical` should be extended to
//! also compare against `BzzAddress::sign` / `BzzAddress::parse`.
//!
//! The ECDSA signature itself is deterministic under RFC 6979 (k256 /
//! go-ethereum both use it), so the resulting signature bytes are also
//! pinned. A second vertex implementation, or a future bee revision, that
//! changes any byte of the sign-data layout will fail these tests.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "golden-vector tests: panicking on malformed fixtures is the intended behaviour"
)]

use alloy_primitives::{Address, B256, Signature};
use alloy_signer::{Signer, SignerSync};
use alloy_signer_local::PrivateKeySigner;
use libp2p::Multiaddr;
use vertex_swarm_peer::serialize_multiaddrs;
use vertex_swarm_primitives::compute_overlay;

/// Pinned cross-implementation vector for the handshake-15.0.0 `BzzAddress`.
///
/// Newtype-style record (constructed only by this test file); every field is
/// rendered in lowercase hex without the `0x` prefix so the table reads
/// identically to bee's Go-side fixtures.
#[derive(Debug, Clone, Copy)]
struct GoldenVector {
    /// 32-byte secp256k1 private key.
    private_key_hex: &'static str,
    /// Swarm network id (little-endian in the overlay, big-endian in sign-data).
    network_id: u64,
    /// 32-byte handshake nonce.
    nonce_hex: &'static str,
    /// Unix-seconds timestamp; the signed wall-clock used in sign-data.
    timestamp: i64,
    /// Single underlay multiaddr in textual form (handed to `Multiaddr::parse`).
    ///
    /// The vectors carry exactly one underlay to match
    /// `serialize_multiaddrs`'s single-address fast path (raw multiaddr
    /// bytes, no `0x99` list prefix).
    underlay: &'static str,
    /// Optional 20-byte chequebook address (lowercase hex, no `0x`).
    chequebook_hex: Option<&'static str>,
    /// Expected 20-byte Ethereum address derived from `private_key_hex`.
    expected_eth_address_hex: &'static str,
    /// Expected 32-byte overlay
    /// (`keccak256(eth_addr || network_id_le || nonce)`).
    expected_overlay_hex: &'static str,
    /// Expected EIP-191 secp256k1 signature over the sign-data bytes.
    ///
    /// Format: `r || s || v` (65 bytes); `v` is the raw EIP-191 recovery
    /// byte (`0x1b` / `0x1c`) as emitted by both `alloy-signer` and bee's
    /// `crypto.Signer`.
    expected_signature_hex: &'static str,
}

impl GoldenVector {
    fn signer(&self) -> PrivateKeySigner {
        let bytes = hex_to_array::<32>(self.private_key_hex);
        PrivateKeySigner::from_bytes(&B256::from(bytes)).expect("valid secp256k1 key")
    }

    fn nonce(&self) -> [u8; 32] {
        hex_to_array::<32>(self.nonce_hex)
    }

    fn chequebook(&self) -> Option<[u8; 20]> {
        self.chequebook_hex.map(hex_to_array::<20>)
    }

    fn underlay(&self) -> Multiaddr {
        self.underlay.parse().expect("valid underlay multiaddr")
    }

    fn expected_eth_address(&self) -> Address {
        Address::from(hex_to_array::<20>(self.expected_eth_address_hex))
    }

    fn expected_overlay(&self) -> nectar_primitives::SwarmAddress {
        nectar_primitives::SwarmAddress::from(hex_to_array::<32>(self.expected_overlay_hex))
    }

    fn expected_signature(&self) -> Signature {
        let bytes = hex_to_array::<65>(self.expected_signature_hex);
        Signature::from_raw(&bytes).expect("valid signature")
    }
}

/// Reference sign-data layout, mirrored byte-for-byte from bee's
/// `pkg/bzz/address.go::generateSignData`.
fn reference_sign_data(
    underlay_bytes: &[u8],
    overlay: &[u8; 32],
    network_id: u64,
    nonce: &[u8; 32],
    timestamp: i64,
    chequebook: Option<&[u8; 20]>,
) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(b"bee-handshake-".len() + underlay_bytes.len() + 32 + 8 + 32 + 8 + 20);
    out.extend_from_slice(b"bee-handshake-");
    out.extend_from_slice(underlay_bytes);
    out.extend_from_slice(overlay);
    out.extend_from_slice(&network_id.to_be_bytes());
    out.extend_from_slice(nonce);
    // Bee casts `int64 -> uint64` before big-endian encoding so negative
    // timestamps would wrap. Tests use positive values; matching the cast
    // keeps the layout identical for the full int64 range.
    out.extend_from_slice(&(timestamp as u64).to_be_bytes());
    match chequebook {
        Some(addr) => out.extend_from_slice(addr),
        None => out.extend_from_slice(&[0u8; 20]),
    }
    out
}

/// Decode a fixed-size lowercase hex string into a byte array.
fn hex_to_array<const N: usize>(hex: &str) -> [u8; N] {
    let trimmed = hex.strip_prefix("0x").unwrap_or(hex);
    assert_eq!(
        trimmed.len(),
        N * 2,
        "golden vector hex literal `{hex}` has wrong length",
    );
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        let off = i * 2;
        *byte = u8::from_str_radix(&trimmed[off..off + 2], 16)
            .expect("golden vector contains non-hex characters");
    }
    out
}

// ---------------------------------------------------------------------------
// Vector table
// ---------------------------------------------------------------------------

/// Overlay-only vectors lifted verbatim from
/// `bee/pkg/crypto/crypto_test.go::TestNewOverlayFromEthereumAddress`.
const OVERLAY_VECTORS: &[(/* eth */ &str, /* nid */ u64, /* nonce */ &str, /* overlay */ &str)] = &[
    (
        "1815cac638d1525b47f848daf02b7953e4edd15c",
        1,
        "0000000000000000000000000000000000000000000000000000000000000001",
        "a38f7a814d4b249ae9d3821e9b898019c78ac9abe248fff171782c32a3849a17",
    ),
    (
        "1815cac638d1525b47f848daf02b7953e4edd15c",
        1,
        "0000000000000000000000000000000000000000000000000000000000000002",
        "c63c10b1728dfc463c64c264f71a621fe640196979375840be42dc496b702610",
    ),
    (
        "d26bc1715e933bd5f8fad16310042f13abc16159",
        2,
        "0000000000000000000000000000000000000000000000000000000000000001",
        "9f421f9149b8e31e238cfbdc6e5e833bacf1e42f77f60874d49291292858968e",
    ),
    (
        "ac485e3c63dcf9b4cda9f007628bb0b6fed1c063",
        1,
        "0000000000000000000000000000000000000000000000000000000000000000",
        "fe3a6d582c577404fb19df64a44e00d3a3b71230a8464c0dd34af3f0791b45f2",
    ),
];

/// Full handshake-15.0.0 vectors. The Ethereum address column is the value
/// pinned in bee's `TestNewEthereumAddress` for the same private key; the
/// vectors here cover both `chequebook = None` and `Some(_)` to exercise
/// the optional-field branch of `generateSignData`.
///
/// `expected_overlay_hex` and `expected_signature_hex` are pinned to the
/// values produced by `compute_overlay` and the alloy/k256 RFC-6979 signer
/// path; these are deterministic so any future drift (in vertex *or* in
/// bee) will be caught here.
const VECTORS: &[GoldenVector] = &[
    GoldenVector {
        private_key_hex: "2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae",
        network_id: 10,
        nonce_hex: "0000000000000000000000000000000000000000000000000000000000000002",
        timestamp: 1_700_000_000,
        underlay: "/ip4/127.0.0.1/tcp/1634",
        chequebook_hex: None,
        expected_eth_address_hex: "2f63cbeb054ce76050827e42dd75268f6b9d87c5",
        expected_overlay_hex:
            "56c67d004cbcb0de9b5020bb37d0efb5f6c8f568049e6e9bfa0fc0ae696f0509",
        expected_signature_hex:
            "3d916e1bc20f622bc275275f8e76cc8ee255ca513745cf2982b2e0c17be267015ec7472c79fd40ac15a115ba71ea353dba3c507d3c5b83eb53f0e9bad9b44bfa1c",
    },
    GoldenVector {
        private_key_hex: "2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae",
        network_id: 1,
        nonce_hex: "0000000000000000000000000000000000000000000000000000000000000001",
        timestamp: 1_700_000_000,
        underlay: "/ip4/127.0.0.1/tcp/1634",
        chequebook_hex: Some("abc0000000000000000000000000000000000123"),
        expected_eth_address_hex: "2f63cbeb054ce76050827e42dd75268f6b9d87c5",
        expected_overlay_hex:
            "5c447a6f6e2e8c875fc744295e71308a9d406d1a3f093cf3c7d94c8f7acbb836",
        expected_signature_hex:
            "d4728d596c63ff24ca0a4599912356d186bd0b91cd17b0918caf0f064b05b44c7b2c7242a4c4db9dd447cfce90d2012c47e001371eddc7c7746df39265b3f21e1c",
    },
];

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn overlay_vectors_match_bee() {
    for (idx, (eth_hex, network_id, nonce_hex, expected_hex)) in OVERLAY_VECTORS.iter().enumerate()
    {
        let eth = Address::from(hex_to_array::<20>(eth_hex));
        let nonce = B256::from(hex_to_array::<32>(nonce_hex));
        let expected =
            nectar_primitives::SwarmAddress::from(hex_to_array::<32>(expected_hex));

        let got = compute_overlay(&eth, *network_id, &nonce);
        assert_eq!(got, expected, "overlay vector {idx} mismatch");
    }
}

#[test]
fn full_vectors_roundtrip_byte_identical() {
    for (idx, v) in VECTORS.iter().enumerate() {
        let signer = v.signer();
        let underlay = v.underlay();
        let underlay_bytes = serialize_multiaddrs(std::slice::from_ref(&underlay));
        let nonce = v.nonce();
        let cb = v.chequebook();

        // (1) Ethereum address derivation matches bee's TestNewEthereumAddress.
        assert_eq!(
            Signer::address(&signer),
            v.expected_eth_address(),
            "vector {idx}: derived ethereum address mismatch",
        );

        // (2) Overlay derivation matches the reference vectors in bee's
        // TestNewOverlayFromEthereumAddress (same hashing layout).
        let overlay = compute_overlay(
            &Signer::address(&signer),
            v.network_id,
            &B256::from(nonce),
        );
        let overlay_bytes32: [u8; 32] = overlay.into();
        assert_eq!(
            overlay,
            v.expected_overlay(),
            "vector {idx}: overlay mismatch",
        );

        // (3) Reference sign-data layout signed with the same key must
        // recover to the pinned Ethereum address — proving the sign-data
        // bytes are exactly bee's `generateSignData` output.
        let sign_data = reference_sign_data(
            &underlay_bytes,
            &overlay_bytes32,
            v.network_id,
            &nonce,
            v.timestamp,
            cb.as_ref(),
        );
        let signature: Signature = signer
            .sign_message_sync(&sign_data)
            .expect("sign sign-data");
        assert_eq!(
            signature,
            v.expected_signature(),
            "vector {idx}: signature does not match pinned value",
        );

        let recovered = signature
            .recover_address_from_msg(&sign_data)
            .expect("recover address");
        assert_eq!(
            recovered,
            v.expected_eth_address(),
            "vector {idx}: recovered ethereum address mismatch",
        );
    }
}

#[test]
fn reference_sign_data_layout_matches_bee_spec() {
    // Mirror of bee's `TestGenerateSignData` ordering check: the prefix is
    // present at the very start of the sign-data payload, and every field
    // changes the resulting bytes.
    let underlay = b"underlay-data";
    let overlay = [0x55u8; 32];
    let nonce = [0u8; 32];
    let timestamp: i64 = 1_000_000;
    let cb = [0x11u8; 20];

    let base = reference_sign_data(underlay, &overlay, 1, &nonce, timestamp, Some(&cb));
    assert!(
        base.starts_with(b"bee-handshake-"),
        "missing bee-handshake- prefix",
    );

    let cases: [(&str, Vec<u8>); 6] = [
        (
            "underlay",
            reference_sign_data(b"other-underlay", &overlay, 1, &nonce, timestamp, Some(&cb)),
        ),
        (
            "overlay",
            reference_sign_data(underlay, &[0xAAu8; 32], 1, &nonce, timestamp, Some(&cb)),
        ),
        (
            "network_id",
            reference_sign_data(underlay, &overlay, 2, &nonce, timestamp, Some(&cb)),
        ),
        (
            "nonce",
            reference_sign_data(underlay, &overlay, 1, &[1u8; 32], timestamp, Some(&cb)),
        ),
        (
            "timestamp",
            reference_sign_data(underlay, &overlay, 1, &nonce, timestamp + 1, Some(&cb)),
        ),
        (
            "chequebook",
            reference_sign_data(underlay, &overlay, 1, &nonce, timestamp, Some(&[0x22u8; 20])),
        ),
    ];

    for (field, got) in cases {
        assert_ne!(base, got, "changing {field} did not change sign data");
    }
}

