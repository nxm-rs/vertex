//! Topology and neighborhood awareness.
//!
//! This module defines the [`Topology`] trait for Kademlia-style routing.
//! All operations use [`OverlayAddress`] (not libp2p `PeerId`) since routing
//! is based on Swarm overlay addresses.

use std::vec::Vec;
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
#[auto_impl::auto_impl(&, Arc)]
pub trait Topology: Send + Sync {
    /// Get our own overlay address.
    fn self_address(&self) -> OverlayAddress;

    /// Get peers within our neighborhood at the given depth.
    fn neighbors(&self, depth: u8) -> Vec<OverlayAddress>;

    /// Check if an address falls within our area of responsibility.
    fn is_responsible_for(&self, address: &ChunkAddress) -> bool;

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
