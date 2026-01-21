//! Kademlia DHT routing table.
//!
//! Implements the Swarm-specific Kademlia routing table which organizes peers
//! by their proximity order (PO) to the local node's overlay address.
//!
//! # Proximity Order
//!
//! The proximity order between two addresses is the number of leading bits they
//! share. For example:
//! - PO 0: First bit differs (addresses in different halves of the address space)
//! - PO 8: First 8 bits match
//! - PO 31: Maximum PO for standard routing (capped)
//!
//! Proximity is calculated using `SwarmAddress::proximity()` from nectar-primitives.
//!
//! # Bins
//!
//! Peers are organized into bins by their proximity order. Bin N contains peers
//! with PO = N to the local address. The routing table maintains a target number
//! of peers per bin (saturation).
//!
//! # Depth
//!
//! The node's "depth" (or radius) is the highest bin with at least one peer.
//! This determines storage responsibility: a node stores chunks whose addresses
//! have PO >= depth with the node's overlay address.

mod peer;

pub use peer::{KademliaPeer, PeerInfo};

use std::collections::HashMap;
use vertex_primitives::OverlayAddress;

/// Maximum proximity order for standard routing (matches nectar-primitives).
///
/// This caps the PO at 31, meaning bins 0-31 are used for routing.
/// For extended proximity (bin balancing), use the full 255.
pub const MAX_PO: u8 = 31;

/// Number of bins in the routing table (MAX_PO + 1).
pub const NUM_BINS: usize = (MAX_PO as usize) + 1;

/// Configuration for the Kademlia routing table.
#[derive(Debug, Clone)]
pub struct KademliaConfig {
    /// Target number of peers per bin.
    pub saturation_target: usize,

    /// Maximum peers per bin before pruning.
    pub max_bin_size: usize,
}

impl Default for KademliaConfig {
    fn default() -> Self {
        Self {
            saturation_target: 4,
            max_bin_size: 16,
        }
    }
}

/// The Kademlia routing table.
///
/// Organizes peers by their proximity order to the local overlay address.
pub struct Kademlia {
    /// Local node's overlay address.
    local: OverlayAddress,

    /// Configuration.
    config: KademliaConfig,

    /// Bins indexed by proximity order.
    /// bins[i] contains peers with PO = i to local address.
    bins: Vec<Vec<KademliaPeer>>,

    /// Index of peers by overlay address for O(1) lookup.
    peers: HashMap<OverlayAddress, usize>, // overlay -> bin index
}

impl Kademlia {
    /// Create a new Kademlia routing table.
    pub fn new(local: OverlayAddress, config: KademliaConfig) -> Self {
        Self {
            local,
            config,
            bins: vec![Vec::new(); NUM_BINS],
            peers: HashMap::new(),
        }
    }

    /// Get the local overlay address.
    pub fn local(&self) -> &OverlayAddress {
        &self.local
    }

    /// Calculate the proximity order to another address.
    ///
    /// Uses `SwarmAddress::proximity()` from nectar-primitives.
    pub fn proximity(&self, other: &OverlayAddress) -> u8 {
        self.local.proximity(other)
    }

    /// Add a peer to the routing table.
    ///
    /// Returns `true` if the peer was added, `false` if already present or bin full.
    pub fn add(&mut self, peer: KademliaPeer) -> bool {
        let overlay = peer.overlay.clone();

        // Don't add ourselves
        if overlay == self.local {
            return false;
        }

        // Check if already present
        if self.peers.contains_key(&overlay) {
            return false;
        }

        let po = self.proximity(&overlay) as usize;

        // Check bin capacity
        if self.bins[po].len() >= self.config.max_bin_size {
            return false;
        }

        self.bins[po].push(peer);
        self.peers.insert(overlay, po);
        true
    }

    /// Remove a peer from the routing table.
    pub fn remove(&mut self, overlay: &OverlayAddress) -> Option<KademliaPeer> {
        let bin_idx = self.peers.remove(overlay)?;

        let bin = &mut self.bins[bin_idx];
        let pos = bin.iter().position(|p| &p.overlay == overlay)?;
        Some(bin.remove(pos))
    }

    /// Get a peer by overlay address.
    pub fn get(&self, overlay: &OverlayAddress) -> Option<&KademliaPeer> {
        let bin_idx = self.peers.get(overlay)?;
        self.bins[*bin_idx]
            .iter()
            .find(|p| &p.overlay == overlay)
    }

    /// Check if a peer is in the routing table.
    pub fn contains(&self, overlay: &OverlayAddress) -> bool {
        self.peers.contains_key(overlay)
    }

    /// Get all peers in a specific bin.
    pub fn bin(&self, po: u8) -> &[KademliaPeer] {
        &self.bins[po as usize]
    }

    /// Get the current depth (radius) of the node.
    ///
    /// The depth is the highest bin index that has at least one peer,
    /// or 0 if no peers are connected.
    pub fn depth(&self) -> u8 {
        for (i, bin) in self.bins.iter().enumerate().rev() {
            if !bin.is_empty() {
                return i as u8;
            }
        }
        0
    }

    /// Get the total number of peers in the routing table.
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    /// Check if the routing table is empty.
    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// Check if a bin is saturated (has enough peers).
    pub fn is_bin_saturated(&self, po: u8) -> bool {
        self.bins[po as usize].len() >= self.config.saturation_target
    }

    /// Get peers that are closest to a target address.
    ///
    /// Returns up to `count` peers, ordered by proximity to the target.
    pub fn closest(&self, target: &OverlayAddress, count: usize) -> Vec<&KademliaPeer> {
        let mut all_peers: Vec<_> = self.bins.iter().flatten().collect();

        // Sort by distance to target using SwarmAddress::distance_cmp
        // distance_cmp returns Greater when first arg is closer, so we reverse for ascending
        all_peers.sort_by(|a, b| target.distance_cmp(&a.overlay, &b.overlay).reverse());

        all_peers.into_iter().take(count).collect()
    }

    /// Iterate over all peers in the routing table.
    pub fn iter(&self) -> impl Iterator<Item = &KademliaPeer> {
        self.bins.iter().flatten()
    }

    /// Get statistics about the routing table.
    pub fn stats(&self) -> KademliaStats {
        let mut bin_counts = Vec::with_capacity(NUM_BINS);
        let mut saturated_bins = 0;

        for bin in &self.bins {
            let count = bin.len();
            bin_counts.push(count);
            if count >= self.config.saturation_target {
                saturated_bins += 1;
            }
        }

        KademliaStats {
            total_peers: self.peers.len(),
            depth: self.depth(),
            saturated_bins,
            bin_counts,
        }
    }
}

/// Statistics about the Kademlia routing table.
#[derive(Debug, Clone)]
pub struct KademliaStats {
    /// Total number of peers.
    pub total_peers: usize,

    /// Current depth (radius).
    pub depth: u8,

    /// Number of saturated bins.
    pub saturated_bins: usize,

    /// Peer count per bin.
    pub bin_counts: Vec<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_overlay(bytes: [u8; 32]) -> OverlayAddress {
        OverlayAddress::new(bytes)
    }

    #[test]
    fn test_add_and_remove() {
        let local = make_overlay([0u8; 32]);
        let mut kad = Kademlia::new(local, KademliaConfig::default());

        // Different first byte = PO 0
        let peer1 = KademliaPeer::new(make_overlay([0x80; 32]));
        assert!(kad.add(peer1.clone()));
        assert_eq!(kad.len(), 1);

        // Same peer again should fail
        assert!(!kad.add(peer1.clone()));
        assert_eq!(kad.len(), 1);

        // Remove
        let removed = kad.remove(&peer1.overlay);
        assert!(removed.is_some());
        assert_eq!(kad.len(), 0);
    }

    #[test]
    fn test_depth() {
        let local = make_overlay([0u8; 32]);
        let mut kad = Kademlia::new(local, KademliaConfig::default());

        assert_eq!(kad.depth(), 0);

        // Add peer at PO 0 (first bit differs)
        let peer_po0 = KademliaPeer::new(make_overlay([0x80; 32]));
        kad.add(peer_po0);
        assert_eq!(kad.depth(), 0);

        // Add peer at PO 7 (first 7 bits match: 0x01 = 00000001)
        let mut bytes = [0u8; 32];
        bytes[0] = 0x01; // 00000001 vs 00000000 = 7 leading zeros match
        let peer_po7 = KademliaPeer::new(make_overlay(bytes));
        kad.add(peer_po7);
        assert_eq!(kad.depth(), 7);
    }

    #[test]
    fn test_dont_add_self() {
        let local = make_overlay([0u8; 32]);
        let mut kad = Kademlia::new(local.clone(), KademliaConfig::default());

        let self_peer = KademliaPeer::new(local);
        assert!(!kad.add(self_peer));
        assert_eq!(kad.len(), 0);
    }
}
