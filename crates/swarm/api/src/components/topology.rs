//! Topology and neighborhood awareness using overlay addresses.

use nectar_primitives::ChunkAddress;
use std::vec::Vec;
use vertex_swarm_primitives::OverlayAddress;

use crate::SwarmIdentity;

/// Neighborhood awareness - who is "close" in the overlay address space.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmTopology: Send + Sync {
    /// The identity type for this topology.
    type Identity: SwarmIdentity;

    /// Get the node's identity.
    fn identity(&self) -> &Self::Identity;

    /// Get our own overlay address.
    fn self_address(&self) -> OverlayAddress {
        self.identity().overlay_address()
    }

    /// Get peers within our neighborhood at the given depth.
    fn neighbors(&self, depth: u8) -> Vec<OverlayAddress>;

    /// Get the current neighborhood depth.
    fn depth(&self) -> u8;

    /// Find peers closest to a given address.
    fn closest_to(&self, address: &ChunkAddress, count: usize) -> Vec<OverlayAddress>;

    /// Add discovered peers (from Hive). May trigger connection evaluation.
    fn add_peers(&self, peers: &[OverlayAddress]);

    /// Should we accept an inbound connection from this peer?
    fn pick(&self, peer: &OverlayAddress, is_full_node: bool) -> bool;

    /// Notify that a peer has connected.
    fn connected(&self, peer: OverlayAddress);

    /// Notify that a peer has disconnected.
    fn disconnected(&self, peer: &OverlayAddress);

    /// Get peers we should try to connect to.
    fn peers_to_connect(&self) -> Vec<OverlayAddress>;
}
