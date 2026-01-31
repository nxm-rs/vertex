//! Proximity-ordered peer storage (PSlice).
//!
//! Peers are organized into bins based on their proximity order (PO).
//! The base address is not stored here - callers provide the PO when adding peers.
//!
//! # Implementation
//!
//! Uses a single `HashMap<OverlayAddress, u8>` for O(1) peer operations,
//! with atomic counters per bin for lock-free statistics.

use std::{
    collections::HashMap,
    sync::atomic::{AtomicUsize, Ordering},
};

use parking_lot::RwLock;
use vertex_swarm_primitives::OverlayAddress;

/// Maximum proximity order for 256-bit addresses.
pub const MAX_PO: u8 = 31;

/// Number of bins (one for each possible PO value 0-31).
const NUM_BINS: usize = 32;

/// Proximity-ordered peer storage.
///
/// Stores peers with their proximity order (PO).
/// Provides O(1) insert/remove/lookup and lock-free bin count queries.
///
/// Does not store the base address - callers provide proximity order when adding.
pub struct PSlice {
    /// Maps peer address to its proximity order. Single lock for all operations.
    peers: RwLock<HashMap<OverlayAddress, u8>>,
    /// Atomic counters per bin for lock-free size queries.
    bin_counts: [AtomicUsize; NUM_BINS],
}

impl Default for PSlice {
    fn default() -> Self {
        Self::new()
    }
}

impl PSlice {
    /// Create a new empty PSlice.
    pub fn new() -> Self {
        Self {
            peers: RwLock::new(HashMap::new()),
            bin_counts: std::array::from_fn(|_| AtomicUsize::new(0)),
        }
    }

    /// Add a peer with its proximity order.
    ///
    /// Returns `true` if the peer was added (not already present).
    pub fn add(&self, peer: OverlayAddress, po: u8) -> bool {
        let mut peers = self.peers.write();

        if peers.contains_key(&peer) {
            return false;
        }

        peers.insert(peer, po);
        self.bin_counts[po as usize].fetch_add(1, Ordering::Relaxed);
        true
    }

    /// Remove a peer.
    ///
    /// Returns `true` if the peer was present and removed.
    pub fn remove(&self, peer: &OverlayAddress) -> bool {
        let mut peers = self.peers.write();

        if let Some(po) = peers.remove(peer) {
            self.bin_counts[po as usize].fetch_sub(1, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    /// Check if a peer exists in the PSlice.
    pub fn exists(&self, peer: &OverlayAddress) -> bool {
        self.peers.read().contains_key(peer)
    }

    /// Get the proximity order of a peer, if present.
    pub fn get_po(&self, peer: &OverlayAddress) -> Option<u8> {
        self.peers.read().get(peer).copied()
    }

    /// Get the number of peers in a specific bin (lock-free).
    pub fn bin_size(&self, po: u8) -> usize {
        self.bin_counts[po as usize].load(Ordering::Relaxed)
    }

    /// Get the total number of peers (lock-free).
    pub fn len(&self) -> usize {
        self.bin_counts
            .iter()
            .map(|c| c.load(Ordering::Relaxed))
            .sum()
    }

    /// Check if empty (lock-free).
    pub fn is_empty(&self) -> bool {
        self.bin_counts
            .iter()
            .all(|c| c.load(Ordering::Relaxed) == 0)
    }

    /// Get all peers in a specific bin.
    pub fn peers_in_bin(&self, po: u8) -> Vec<OverlayAddress> {
        self.peers
            .read()
            .iter()
            .filter(|&(_, &p)| p == po)
            .map(|(&addr, _)| addr)
            .collect()
    }

    /// Iterate over all peers with their proximity order, from shallowest to deepest.
    pub fn iter_by_proximity(&self) -> impl Iterator<Item = (u8, OverlayAddress)> {
        let mut peers: Vec<_> = self
            .peers
            .read()
            .iter()
            .map(|(&addr, &po)| (po, addr))
            .collect();
        peers.sort_by_key(|(po, _)| *po);
        peers.into_iter()
    }

    /// Iterate over all peers with their proximity order, from deepest to shallowest.
    pub fn iter_by_proximity_desc(&self) -> impl Iterator<Item = (u8, OverlayAddress)> {
        let mut peers: Vec<_> = self
            .peers
            .read()
            .iter()
            .map(|(&addr, &po)| (po, addr))
            .collect();
        peers.sort_by_key(|(po, _)| std::cmp::Reverse(*po));
        peers.into_iter()
    }

    /// Get all peers as a flat vector.
    pub fn all_peers(&self) -> Vec<OverlayAddress> {
        self.peers.read().keys().copied().collect()
    }

    /// Get bin sizes as an array (lock-free).
    pub fn bin_sizes(&self) -> [usize; NUM_BINS] {
        std::array::from_fn(|i| self.bin_counts[i].load(Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pslice_add_remove() {
        let pslice = PSlice::new();

        let peer1 = OverlayAddress::from([0x80; 32]);
        let peer2 = OverlayAddress::from([0x40; 32]);

        assert!(pslice.add(peer1, 0)); // PO 0
        assert!(!pslice.add(peer1, 0)); // Already exists
        assert!(pslice.add(peer2, 1)); // PO 1

        assert_eq!(pslice.len(), 2);
        assert!(pslice.exists(&peer1));
        assert!(pslice.exists(&peer2));

        assert!(pslice.remove(&peer1));
        assert!(!pslice.remove(&peer1)); // Already removed

        assert_eq!(pslice.len(), 1);
        assert!(!pslice.exists(&peer1));
        assert!(pslice.exists(&peer2));
    }

    #[test]
    fn test_pslice_bin_size() {
        let pslice = PSlice::new();

        let peer1 = OverlayAddress::from([0x80; 32]);
        let peer2 = OverlayAddress::from([0xc0; 32]);
        let peer3 = OverlayAddress::from([0x40; 32]);

        pslice.add(peer1, 0); // PO 0
        pslice.add(peer2, 0); // PO 0
        pslice.add(peer3, 1); // PO 1

        assert_eq!(pslice.bin_size(0), 2);
        assert_eq!(pslice.bin_size(1), 1);
        assert_eq!(pslice.bin_size(2), 0);
    }

    #[test]
    fn test_pslice_get_po() {
        let pslice = PSlice::new();

        let peer = OverlayAddress::from([0x80; 32]);
        pslice.add(peer, 5);

        assert_eq!(pslice.get_po(&peer), Some(5));
        assert_eq!(pslice.get_po(&OverlayAddress::from([0x00; 32])), None);
    }

    #[test]
    fn test_pslice_iter_by_proximity() {
        let pslice = PSlice::new();

        let peer0 = OverlayAddress::from([0x80; 32]);
        let peer1 = OverlayAddress::from([0x40; 32]);
        let peer2 = OverlayAddress::from([0x20; 32]);

        pslice.add(peer2, 2);
        pslice.add(peer0, 0);
        pslice.add(peer1, 1);

        let collected: Vec<_> = pslice.iter_by_proximity().collect();
        assert_eq!(collected.len(), 3);
        assert_eq!(collected[0].0, 0); // First is PO 0
        assert_eq!(collected[1].0, 1); // Second is PO 1
        assert_eq!(collected[2].0, 2); // Third is PO 2
    }

    #[test]
    fn test_pslice_iter_by_proximity_desc() {
        let pslice = PSlice::new();

        let peer0 = OverlayAddress::from([0x80; 32]);
        let peer1 = OverlayAddress::from([0x40; 32]);
        let peer2 = OverlayAddress::from([0x20; 32]);

        pslice.add(peer0, 0);
        pslice.add(peer1, 1);
        pslice.add(peer2, 2);

        let collected: Vec<_> = pslice.iter_by_proximity_desc().collect();
        assert_eq!(collected.len(), 3);
        assert_eq!(collected[0].0, 2); // First is PO 2 (deepest)
        assert_eq!(collected[1].0, 1); // Second is PO 1
        assert_eq!(collected[2].0, 0); // Third is PO 0 (shallowest)
    }

    #[test]
    fn test_pslice_bin_sizes() {
        let pslice = PSlice::new();

        let peer1 = OverlayAddress::from([0x80; 32]);
        let peer2 = OverlayAddress::from([0x40; 32]);
        let peer3 = OverlayAddress::from([0x20; 32]);

        pslice.add(peer1, 0);
        pslice.add(peer2, 1);
        pslice.add(peer3, 2);

        let sizes = pslice.bin_sizes();
        assert_eq!(sizes[0], 1);
        assert_eq!(sizes[1], 1);
        assert_eq!(sizes[2], 1);
        assert_eq!(sizes[3], 0);
    }

    #[test]
    fn test_pslice_peers_in_bin() {
        let pslice = PSlice::new();

        let peer1 = OverlayAddress::from([0x80; 32]);
        let peer2 = OverlayAddress::from([0xc0; 32]);
        let peer3 = OverlayAddress::from([0x40; 32]);

        pslice.add(peer1, 0);
        pslice.add(peer2, 0);
        pslice.add(peer3, 1);

        let bin0_peers = pslice.peers_in_bin(0);
        assert_eq!(bin0_peers.len(), 2);
        assert!(bin0_peers.contains(&peer1));
        assert!(bin0_peers.contains(&peer2));

        let bin1_peers = pslice.peers_in_bin(1);
        assert_eq!(bin1_peers.len(), 1);
        assert!(bin1_peers.contains(&peer3));
    }

    #[test]
    fn test_pslice_default() {
        let pslice = PSlice::default();
        assert!(pslice.is_empty());
        assert_eq!(pslice.len(), 0);
    }
}
