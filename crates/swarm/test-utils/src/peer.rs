//! Test helpers for creating peer fixtures.

use alloy_primitives::{Address, B256, Signature, U256};
use libp2p::PeerId;
use nectar_primitives::SwarmAddress;
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_primitives::OverlayAddress;

/// Create a test overlay address from a single byte.
///
/// The byte is repeated to fill all 32 bytes.
///
/// # Example
///
/// ```ignore
/// use vertex_swarm_test_utils::test_overlay;
///
/// let overlay = test_overlay(0x42);
/// // overlay == [0x42, 0x42, 0x42, ..., 0x42]
/// ```
pub fn test_overlay(n: u8) -> OverlayAddress {
    OverlayAddress::from(B256::repeat_byte(n))
}

/// Create a simple test peer overlay address.
///
/// Returns `OverlayAddress::from([1u8; 32])`.
///
/// This is the most commonly used test peer address in the codebase.
pub fn test_peer() -> OverlayAddress {
    OverlayAddress::from([1u8; 32])
}

/// Create a deterministic PeerId from a byte.
///
/// Uses ed25519 key derivation from `[n; 32]` bytes.
/// The same `n` always produces the same PeerId, making tests reproducible.
///
/// # Example
///
/// ```ignore
/// use vertex_swarm_test_utils::test_peer_id;
///
/// let peer1 = test_peer_id(1);
/// let peer2 = test_peer_id(1);
/// let peer3 = test_peer_id(2);
///
/// assert_eq!(peer1, peer2);  // Same input = same PeerId
/// assert_ne!(peer1, peer3);  // Different input = different PeerId
/// ```
pub fn test_peer_id(n: u8) -> PeerId {
    let bytes = [n; 32];
    let key = libp2p::identity::ed25519::SecretKey::try_from_bytes(bytes)
        .expect("32 bytes is valid ed25519 secret key");
    let keypair = libp2p::identity::Keypair::from(libp2p::identity::ed25519::Keypair::from(key));
    keypair.public().to_peer_id()
}

/// Create a test SwarmPeer with deterministic values.
///
/// - Overlay address: all bytes = `n`
/// - Multiaddr: `/ip4/127.0.0.{n}/tcp/1634/p2p/{peer_id}`
/// - PeerId: deterministic from `n` via `test_peer_id(n)`
/// - Signature: test signature
/// - Nonce: zero
/// - Ethereum address: zero
///
/// # Example
///
/// ```ignore
/// use vertex_swarm_test_utils::test_swarm_peer;
///
/// let peer = test_swarm_peer(5);
/// // peer has overlay [5, 5, 5, ..., 5]
/// // peer has multiaddr /ip4/127.0.0.5/tcp/1634/p2p/{peer_id}
/// ```
pub fn test_swarm_peer(n: u8) -> SwarmPeer {
    let overlay = B256::repeat_byte(n);
    let peer_id = test_peer_id(n);
    let multiaddrs = vec![
        format!("/ip4/127.0.0.{}/tcp/1634/p2p/{}", n, peer_id)
            .parse()
            .expect("valid multiaddr"),
    ];
    SwarmPeer::from_validated(
        multiaddrs,
        Signature::test_signature(),
        overlay,
        B256::ZERO,
        Address::ZERO,
    )
}

/// Create a SwarmAddress with a specific first byte.
///
/// The first byte is set to `byte`, remaining bytes are zero.
/// This is useful for Kademlia routing tests where you need to control
/// which bin a peer falls into.
///
/// # Example
///
/// ```ignore
/// use vertex_swarm_test_utils::make_overlay;
///
/// let overlay = make_overlay(0x80);
/// // overlay == [0x80, 0x00, 0x00, ..., 0x00]
/// ```
pub fn make_overlay(byte: u8) -> SwarmAddress {
    SwarmAddress::with_first_byte(byte)
}

/// Create a minimal SwarmPeer for testing (no multiaddrs).
///
/// Useful when you only need the overlay address and don't care about
/// network connectivity.
pub fn make_swarm_peer_minimal(overlay_byte: u8) -> SwarmPeer {
    let overlay = make_overlay(overlay_byte);
    SwarmPeer::from_validated(
        vec![],
        Signature::new(U256::ZERO, U256::ZERO, false),
        B256::from_slice(overlay.as_slice()),
        B256::ZERO,
        Address::ZERO,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_peer_id_deterministic() {
        let id1 = test_peer_id(42);
        let id2 = test_peer_id(42);
        let id3 = test_peer_id(43);

        assert_eq!(id1, id2, "same input should produce same PeerId");
        assert_ne!(id1, id3, "different input should produce different PeerId");
    }

    #[test]
    fn test_overlay_helpers() {
        let overlay = test_overlay(0xff);
        assert_eq!(overlay.as_slice(), &[0xff; 32]);

        let peer = test_peer();
        assert_eq!(peer.as_slice(), &[1u8; 32]);
    }

    #[test]
    fn test_swarm_peer_creation() {
        let peer = test_swarm_peer(5);
        assert_eq!(peer.overlay().as_slice(), &[5u8; 32]);
        assert_eq!(peer.multiaddrs().len(), 1);

        // Verify multiaddr contains /p2p/ component
        let addr = peer.multiaddrs().first().unwrap();
        let has_p2p = addr
            .iter()
            .any(|p| matches!(p, libp2p::multiaddr::Protocol::P2p(_)));
        assert!(has_p2p, "multiaddr should contain /p2p/ component");
    }

    #[test]
    fn test_make_overlay_first_byte() {
        let overlay = make_overlay(0x80);
        assert_eq!(overlay.as_slice()[0], 0x80);
        assert!(overlay.as_slice()[1..].iter().all(|&b| b == 0));
    }
}
