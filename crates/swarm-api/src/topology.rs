//! Topology and neighborhood awareness.
//!
//! This module defines the [`Topology`] trait for Kademlia-style routing.
//! All operations use [`OverlayAddress`] (not libp2p `PeerId`) since routing
//! is based on Swarm overlay addresses.

use alloc::vec::Vec;
use vertex_primitives::{ChunkAddress, OverlayAddress};

/// Neighborhood awareness - who is "close" in the overlay address space.
///
/// This is the abstract concept of proximity in a distributed hash table.
/// Kademlia is one implementation, but the API doesn't assume any specific algorithm.
///
/// # Overlay Addresses
///
/// All methods use [`OverlayAddress`] (32-byte Swarm address) for peer identification.
/// The overlay address determines routing proximity via XOR distance.
/// The mapping from overlay to underlay (libp2p `PeerId`) happens in the net layer.
///
/// # Implementations
///
/// - Kademlia-based topology (standard Swarm)
/// - Custom topologies for testing or specialized networks
pub trait Topology: Send + Sync {
    /// Get our own overlay address.
    ///
    /// Every node has a unique overlay address derived from its Ethereum address.
    /// This is fundamental for determining responsibility and routing.
    fn self_address(&self) -> OverlayAddress;

    /// Get peers within our neighborhood at the given depth.
    ///
    /// Returns overlay addresses of peers in our neighborhood.
    fn neighbors(&self, depth: u8) -> Vec<OverlayAddress>;

    /// Check if an address falls within our area of responsibility.
    fn is_responsible_for(&self, address: &ChunkAddress) -> bool;

    /// Get the current neighborhood depth.
    fn depth(&self) -> u8;

    /// Find peers closest to a given address.
    ///
    /// Returns overlay addresses sorted by proximity to `address`.
    fn closest_to(&self, address: &ChunkAddress, count: usize) -> Vec<OverlayAddress>;
}
