//! Proximity-ordered peer storage (PSlice).
//!
//! Peers are organized into bins based on their proximity order (PO) relative
//! to the base address. PO is calculated using `OverlayAddress::proximity()`.

use parking_lot::RwLock;
use vertex_primitives::OverlayAddress;

/// Maximum proximity order for 256-bit addresses.
pub const MAX_PO: u8 = 31;

/// Number of bins (one for each possible PO value 0-31).
const NUM_BINS: usize = 32;

/// Proximity-ordered peer storage.
///
/// Stores peers in bins based on their proximity order (PO) relative to the
/// base address. Each bin contains peers with the same PO value.
pub struct PSlice {
    base: OverlayAddress,
    bins: [RwLock<Vec<OverlayAddress>>; NUM_BINS],
}

impl PSlice {
    /// Create a new PSlice with the given base address.
    pub fn new(base: OverlayAddress) -> Self {
        Self {
            base,
            bins: std::array::from_fn(|_| RwLock::new(Vec::new())),
        }
    }

    /// Get the base address.
    pub fn base(&self) -> OverlayAddress {
        self.base
    }

    /// Calculate the proximity order between the base and another address.
    pub fn proximity(&self, other: &OverlayAddress) -> u8 {
        self.base.proximity(other)
    }

    /// Add a peer to the appropriate bin.
    ///
    /// Returns `true` if the peer was added (not already present).
    pub fn add(&self, peer: OverlayAddress) -> bool {
        if peer == self.base {
            return false;
        }

        let po = self.proximity(&peer) as usize;
        let mut bin = self.bins[po].write();

        if bin.contains(&peer) {
            return false;
        }

        bin.push(peer);
        true
    }

    /// Remove a peer from its bin.
    ///
    /// Returns `true` if the peer was present and removed.
    pub fn remove(&self, peer: &OverlayAddress) -> bool {
        let po = self.proximity(peer) as usize;
        let mut bin = self.bins[po].write();

        if let Some(idx) = bin.iter().position(|p| p == peer) {
            bin.swap_remove(idx);
            true
        } else {
            false
        }
    }

    /// Check if a peer exists in the PSlice.
    pub fn exists(&self, peer: &OverlayAddress) -> bool {
        let po = self.proximity(peer) as usize;
        self.bins[po].read().contains(peer)
    }

    /// Get the number of peers in a specific bin.
    pub fn bin_size(&self, po: u8) -> usize {
        self.bins[po as usize].read().len()
    }

    /// Get the total number of peers.
    pub fn len(&self) -> usize {
        self.bins.iter().map(|b| b.read().len()).sum()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.bins.iter().all(|b| b.read().is_empty())
    }

    /// Get all peers in a specific bin.
    pub fn peers_in_bin(&self, po: u8) -> Vec<OverlayAddress> {
        self.bins[po as usize].read().clone()
    }

    /// Iterate over all peers with their proximity order, from shallowest to deepest.
    pub fn iter_by_proximity(&self) -> impl Iterator<Item = (u8, OverlayAddress)> + '_ {
        (0..NUM_BINS as u8).flat_map(|po| {
            self.bins[po as usize]
                .read()
                .clone()
                .into_iter()
                .map(move |peer| (po, peer))
        })
    }

    /// Iterate over all peers with their proximity order, from deepest to shallowest.
    pub fn iter_by_proximity_desc(&self) -> impl Iterator<Item = (u8, OverlayAddress)> + '_ {
        (0..NUM_BINS as u8).rev().flat_map(|po| {
            self.bins[po as usize]
                .read()
                .clone()
                .into_iter()
                .map(move |peer| (po, peer))
        })
    }

    /// Get all peers as a flat vector.
    pub fn all_peers(&self) -> Vec<OverlayAddress> {
        self.bins.iter().flat_map(|b| b.read().clone()).collect()
    }

    /// Get bin sizes as an array [bin0_size, bin1_size, ..., bin31_size].
    pub fn bin_sizes(&self) -> [usize; NUM_BINS] {
        std::array::from_fn(|i| self.bins[i].read().len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr_from_bytes(bytes: [u8; 32]) -> OverlayAddress {
        OverlayAddress::from(bytes)
    }

    #[test]
    fn test_pslice_add_remove() {
        let base = addr_from_bytes([0x00; 32]);
        let pslice = PSlice::new(base);

        let peer1 = addr_from_bytes([0x80; 32]); // PO 0
        let peer2 = addr_from_bytes([0x40; 32]); // PO 1

        assert!(pslice.add(peer1));
        assert!(!pslice.add(peer1)); // Already exists
        assert!(pslice.add(peer2));

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
        let base = addr_from_bytes([0x00; 32]);
        let pslice = PSlice::new(base);

        let peer1 = addr_from_bytes([0x80; 32]); // PO 0
        let peer2 = addr_from_bytes([0xc0; 32]); // PO 0
        let peer3 = addr_from_bytes([0x40; 32]); // PO 1

        pslice.add(peer1);
        pslice.add(peer2);
        pslice.add(peer3);

        assert_eq!(pslice.bin_size(0), 2);
        assert_eq!(pslice.bin_size(1), 1);
        assert_eq!(pslice.bin_size(2), 0);
    }

    #[test]
    fn test_pslice_cannot_add_self() {
        let base = addr_from_bytes([0x42; 32]);
        let pslice = PSlice::new(base);

        assert!(!pslice.add(base));
        assert_eq!(pslice.len(), 0);
    }

    #[test]
    fn test_pslice_iter_by_proximity() {
        let base = addr_from_bytes([0x00; 32]);
        let pslice = PSlice::new(base);

        let peer0 = addr_from_bytes([0x80; 32]); // PO 0
        let peer1 = addr_from_bytes([0x40; 32]); // PO 1
        let peer2 = addr_from_bytes([0x20; 32]); // PO 2

        pslice.add(peer2);
        pslice.add(peer0);
        pslice.add(peer1);

        let collected: Vec<_> = pslice.iter_by_proximity().collect();
        assert_eq!(collected.len(), 3);
        assert_eq!(collected[0].0, 0); // First is PO 0
        assert_eq!(collected[1].0, 1); // Second is PO 1
        assert_eq!(collected[2].0, 2); // Third is PO 2
    }
}
