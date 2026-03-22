//! Trait for resolving overlay addresses to peer IDs.

use libp2p::PeerId;
use vertex_swarm_primitives::OverlayAddress;

/// Resolves overlay addresses to libp2p peer IDs.
///
/// The node layer provides a concrete implementation backed by the topology's
/// connection registry. This trait decouples the client behaviour from topology
/// internals (generic identity types, dial reason parameterisation).
pub trait PeerAddressResolver: Send + Sync {
    /// Look up the `PeerId` currently associated with `overlay`, if connected.
    fn peer_id_for_overlay(&self, overlay: &OverlayAddress) -> Option<PeerId>;
}
