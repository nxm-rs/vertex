//! Topology and neighborhood awareness using overlay addresses.

use nectar_primitives::ChunkAddress;
use std::vec::Vec;

use vertex_swarm_primitives::OverlayAddress;

use crate::SwarmIdentity;

// Re-export hex for use in default impl
use hex;

/// Neighborhood awareness and topology status.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmTopology: Send + Sync {
    /// The identity type for this topology.
    type Identity: SwarmIdentity;

    /// Get the node's identity.
    fn identity(&self) -> &Self::Identity;

    /// Get the current neighborhood depth.
    fn depth(&self) -> u8;

    /// Get peers within our neighborhood at the given depth.
    fn neighbors(&self, depth: u8) -> Vec<OverlayAddress>;

    /// Find peers closest to a given address.
    fn closest_to(&self, address: &ChunkAddress, count: usize) -> Vec<OverlayAddress>;

    /// Get the count of currently connected peers.
    fn connected_peers_count(&self) -> usize;

    /// Get the count of known (discovered but not necessarily connected) peers.
    fn known_peers_count(&self) -> usize;

    /// Get the count of pending connection attempts.
    fn pending_connections_count(&self) -> usize;

    /// Get bin sizes for each proximity order (0-31).
    ///
    /// Returns a vector of `(connected, known)` tuples, one per bin.
    fn bin_sizes(&self) -> Vec<(usize, usize)>;

    /// Get connected peer overlay addresses in a specific bin.
    ///
    /// Returns hex-encoded overlay addresses.
    fn connected_peers_in_bin(&self, po: u8) -> Vec<String>;

    /// Get the node's overlay address as a hex-encoded string.
    fn overlay_address(&self) -> String {
        hex::encode(self.identity().overlay_address().as_slice())
    }
}
