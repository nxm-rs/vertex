//! Peer resolution trait.

use vertex_swarm_primitives::OverlayAddress;

/// Resolve an overlay address to a previously verified peer.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmPeerResolver: Send + Sync + 'static {
    /// The peer type returned by resolution.
    type Peer: Clone + Send + Sync + 'static;

    /// Look up a peer by overlay address.
    fn get_swarm_peer(&self, overlay: &OverlayAddress) -> Option<Self::Peer>;
}
