//! Read-only peer resolution trait.

use libp2p::PeerId;

/// Read-only peer resolution (PeerId ↔ application-level Id).
///
/// Provides a narrow, read-only view into the registry for consumers
/// that only need peer lookups without mutation access.
pub trait PeerResolver: Send + Sync + 'static {
    type Id;

    /// Resolve a PeerId to its application-level Id (only if handshake is complete).
    fn resolve_id(&self, peer_id: &PeerId) -> Option<Self::Id>;

    /// Resolve an application-level Id back to its PeerId.
    fn resolve_peer_id(&self, id: &Self::Id) -> Option<PeerId>;
}
