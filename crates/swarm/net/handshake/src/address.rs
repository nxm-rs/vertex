//! Address provider trait for NAT-aware address selection.

use std::sync::Arc;

use libp2p::{Multiaddr, PeerId};

/// Provides addresses for handshake based on peer context.
///
/// Implementations select appropriate addresses to advertise based on the
/// remote peer's network location (e.g., public vs private, same subnet, etc.).
pub trait AddressProvider: Send + Sync {
    /// Get addresses to advertise to a peer based on their address.
    fn addresses_for_peer(&self, peer_addr: &Multiaddr) -> Vec<Multiaddr>;

    /// Get local peer ID for observed address validation.
    fn local_peer_id(&self) -> Option<&PeerId>;
}

impl<T: AddressProvider> AddressProvider for Arc<T> {
    fn addresses_for_peer(&self, peer_addr: &Multiaddr) -> Vec<Multiaddr> {
        (**self).addresses_for_peer(peer_addr)
    }

    fn local_peer_id(&self) -> Option<&PeerId> {
        (**self).local_peer_id()
    }
}

/// No-op address provider that returns empty addresses.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoAddresses;

impl AddressProvider for NoAddresses {
    fn addresses_for_peer(&self, _peer_addr: &Multiaddr) -> Vec<Multiaddr> {
        Vec::new()
    }

    fn local_peer_id(&self) -> Option<&PeerId> {
        None
    }
}
