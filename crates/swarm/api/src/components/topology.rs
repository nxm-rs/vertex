//! Topology and neighborhood awareness using overlay addresses.

use nectar_primitives::ChunkAddress;
use std::vec::Vec;

use crate::SwarmIdentity;
use vertex_swarm_primitives::OverlayAddress;

/// Connection statistics for topology monitoring.
#[auto_impl::auto_impl(&, Arc)]
pub trait TopologyStats: Send + Sync {
    /// Get the count of currently connected peers.
    fn connected_peers_count(&self) -> usize;

    /// Get the count of known (discovered but not connected) peers.
    fn known_peers_count(&self) -> usize;

    /// Get the count of pending connection attempts.
    fn pending_connections_count(&self) -> usize;
}

/// Neighborhood awareness and topology status.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmTopology: Send + Sync {
    /// The identity type for this topology.
    type Identity: SwarmIdentity;

    /// Get the identity.
    fn identity(&self) -> &Self::Identity;

    /// Get the current neighborhood depth.
    fn depth(&self) -> u8;

    /// Get peers within our neighborhood at the given depth.
    fn neighbors(&self, depth: u8) -> Vec<OverlayAddress>;

    /// Find peers closest to a given address.
    fn closest_to(&self, address: &ChunkAddress, count: usize) -> Vec<OverlayAddress>;

    /// Get bin sizes for each proximity order (0-31).
    ///
    /// Returns a vector of `(connected, known)` tuples, one per bin.
    fn bin_sizes(&self) -> Vec<(usize, usize)>;

    /// Get connected peer overlay addresses in a specific bin (hex-encoded).
    fn connected_peers_in_bin(&self, po: u8) -> Vec<String>;

    /// Get the node's overlay address (hex-encoded).
    fn overlay_address(&self) -> String {
        self.identity().overlay_address().to_string()
    }
}
