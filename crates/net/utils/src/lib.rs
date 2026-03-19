//! Multiaddr utilities for peer identification and address handling.

use libp2p::{PeerId, multiaddr::Protocol};

/// Extract the `PeerId` from a multiaddr's trailing `/p2p/` component.
///
/// Per libp2p convention, `/p2p/` is always the last protocol component
/// in a well-formed multiaddr.
pub fn extract_peer_id(addr: &libp2p::Multiaddr) -> Option<PeerId> {
    match addr.iter().last() {
        Some(Protocol::P2p(id)) => Some(id),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_peer_id_from_valid_multiaddr() {
        let key = libp2p::identity::ed25519::SecretKey::try_from_bytes([1u8; 32]).unwrap();
        let keypair = libp2p::identity::ed25519::Keypair::from(key);
        let peer_id = PeerId::from_public_key(&libp2p::identity::PublicKey::from(keypair.public()));

        let addr: libp2p::Multiaddr = format!("/ip4/1.2.3.4/tcp/1634/p2p/{peer_id}")
            .parse()
            .unwrap();

        assert_eq!(extract_peer_id(&addr), Some(peer_id));
    }

    #[test]
    fn returns_none_without_p2p_component() {
        let addr: libp2p::Multiaddr = "/ip4/1.2.3.4/tcp/1634".parse().unwrap();
        assert_eq!(extract_peer_id(&addr), None);
    }

    #[test]
    fn returns_none_for_empty_multiaddr() {
        let addr = libp2p::Multiaddr::empty();
        assert_eq!(extract_peer_id(&addr), None);
    }
}
