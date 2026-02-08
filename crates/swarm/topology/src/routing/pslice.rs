//! Proximity-ordered peer storage (PSlice).

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use parking_lot::RwLock;
use vertex_swarm_primitives::OverlayAddress;

/// Proximity-ordered peer storage with cached sorted lists.
///
/// Peers are stored per-bin for O(1) bin lookups. Sorted lists are cached
/// and invalidated only when peers are added or removed. The number of bins
/// is determined at runtime from the network's max proximity order.
pub struct PSlice {
    /// Peers organized by proximity order bin. Each bin is a Vec of peers.
    bins: RwLock<Vec<Vec<OverlayAddress>>>,
    /// Lock-free bin counts for fast size queries.
    bin_counts: Vec<AtomicUsize>,
    /// Cache generation counter. Incremented on every mutation.
    generation: AtomicU64,
    /// Cached sorted list (ascending by PO) with its generation.
    cache_asc: RwLock<CachedList>,
    /// Cached sorted list (descending by PO) with its generation.
    cache_desc: RwLock<CachedList>,
}

/// A cached sorted peer list with its generation stamp.
struct CachedList {
    generation: u64,
    peers: Vec<(u8, OverlayAddress)>,
}

impl Default for CachedList {
    fn default() -> Self {
        Self {
            generation: u64::MAX, // Invalid generation forces rebuild on first access
            peers: Vec::new(),
        }
    }
}

impl PSlice {
    /// Create a new PSlice with the given maximum proximity order.
    ///
    /// The number of bins will be `max_po + 1` (e.g., max_po=31 gives 32 bins).
    pub(crate) fn new(max_po: u8) -> Self {
        let num_bins = (max_po as usize) + 1;
        Self {
            bins: RwLock::new((0..num_bins).map(|_| Vec::new()).collect()),
            bin_counts: (0..num_bins).map(|_| AtomicUsize::new(0)).collect(),
            generation: AtomicU64::new(0),
            cache_asc: RwLock::new(CachedList::default()),
            cache_desc: RwLock::new(CachedList::default()),
        }
    }

    /// Returns true if the peer was added (not already present).
    pub(crate) fn add(&self, peer: OverlayAddress, po: u8) -> bool {
        let mut bins = self.bins.write();
        let bin = &mut bins[po as usize];

        if bin.contains(&peer) {
            return false;
        }

        bin.push(peer);
        self.bin_counts[po as usize].fetch_add(1, Ordering::Relaxed);
        self.generation.fetch_add(1, Ordering::Release);
        true
    }

    /// Returns true if the peer was present and removed.
    pub(crate) fn remove(&self, peer: &OverlayAddress) -> bool {
        let mut bins = self.bins.write();

        for (po, bin) in bins.iter_mut().enumerate() {
            if let Some(idx) = bin.iter().position(|p| p == peer) {
                bin.swap_remove(idx);
                self.bin_counts[po].fetch_sub(1, Ordering::Relaxed);
                self.generation.fetch_add(1, Ordering::Release);
                return true;
            }
        }
        false
    }

    pub(crate) fn exists(&self, peer: &OverlayAddress) -> bool {
        let bins = self.bins.read();
        bins.iter().any(|bin| bin.contains(peer))
    }

    pub(crate) fn bin_size(&self, po: u8) -> usize {
        self.bin_counts[po as usize].load(Ordering::Relaxed)
    }

    pub(crate) fn len(&self) -> usize {
        self.bin_counts
            .iter()
            .map(|c| c.load(Ordering::Relaxed))
            .sum()
    }

    pub(crate) fn peers_in_bin(&self, po: u8) -> Vec<OverlayAddress> {
        self.bins.read()[po as usize].clone()
    }

    /// Iterate from shallowest to deepest proximity order.
    pub(crate) fn iter_by_proximity(&self) -> impl Iterator<Item = (u8, OverlayAddress)> {
        self.get_sorted_asc().into_iter()
    }

    /// Iterate from deepest to shallowest proximity order.
    pub(crate) fn iter_by_proximity_desc(&self) -> impl Iterator<Item = (u8, OverlayAddress)> {
        self.get_sorted_desc().into_iter()
    }

    pub(crate) fn all_peers(&self) -> Vec<OverlayAddress> {
        let bins = self.bins.read();
        bins.iter().flat_map(|bin| bin.iter().copied()).collect()
    }

    pub(crate) fn bin_sizes(&self) -> Vec<usize> {
        self.bin_counts
            .iter()
            .map(|c| c.load(Ordering::Relaxed))
            .collect()
    }

    /// Get the cached sorted list (ascending). Rebuilds if stale.
    fn get_sorted_asc(&self) -> Vec<(u8, OverlayAddress)> {
        let current_gen = self.generation.load(Ordering::Acquire);

        // Fast path: check if cache is valid
        {
            let cache = self.cache_asc.read();
            if cache.generation == current_gen {
                return cache.peers.clone();
            }
        }

        // Slow path: rebuild cache
        let mut cache = self.cache_asc.write();

        // Double-check after acquiring write lock
        let current = self.generation.load(Ordering::Acquire);
        if cache.generation == current {
            return cache.peers.clone();
        }

        let bins = self.bins.read();
        let mut peers = Vec::with_capacity(self.len());
        for (po, bin) in bins.iter().enumerate() {
            for &peer in bin {
                peers.push((po as u8, peer));
            }
        }
        // Already sorted by construction (bins are in PO order)

        cache.peers = peers.clone();
        cache.generation = current;
        peers
    }

    /// Get the cached sorted list (descending). Rebuilds if stale.
    fn get_sorted_desc(&self) -> Vec<(u8, OverlayAddress)> {
        let current_gen = self.generation.load(Ordering::Acquire);

        // Fast path: check if cache is valid
        {
            let cache = self.cache_desc.read();
            if cache.generation == current_gen {
                return cache.peers.clone();
            }
        }

        // Slow path: rebuild cache
        let mut cache = self.cache_desc.write();

        // Double-check after acquiring write lock
        let current = self.generation.load(Ordering::Acquire);
        if cache.generation == current {
            return cache.peers.clone();
        }

        let bins = self.bins.read();
        let mut peers = Vec::with_capacity(self.len());
        for (po, bin) in bins.iter().enumerate().rev() {
            for &peer in bin {
                peers.push((po as u8, peer));
            }
        }
        // Already sorted descending by construction

        cache.peers = peers.clone();
        cache.generation = current;
        peers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Standard max_po for 256-bit addresses.
    const TEST_MAX_PO: u8 = 31;

    #[test]
    fn test_pslice_add_remove() {
        let pslice = PSlice::new(TEST_MAX_PO);

        let peer1 = OverlayAddress::from([0x80; 32]);
        let peer2 = OverlayAddress::from([0x40; 32]);

        assert!(pslice.add(peer1, 0));
        assert!(!pslice.add(peer1, 0));
        assert!(pslice.add(peer2, 1));

        assert_eq!(pslice.len(), 2);
        assert!(pslice.exists(&peer1));
        assert!(pslice.exists(&peer2));

        assert!(pslice.remove(&peer1));
        assert!(!pslice.remove(&peer1));

        assert_eq!(pslice.len(), 1);
        assert!(!pslice.exists(&peer1));
        assert!(pslice.exists(&peer2));
    }

    #[test]
    fn test_pslice_bin_size() {
        let pslice = PSlice::new(TEST_MAX_PO);

        let peer1 = OverlayAddress::from([0x80; 32]);
        let peer2 = OverlayAddress::from([0xc0; 32]);
        let peer3 = OverlayAddress::from([0x40; 32]);

        pslice.add(peer1, 0);
        pslice.add(peer2, 0);
        pslice.add(peer3, 1);

        assert_eq!(pslice.bin_size(0), 2);
        assert_eq!(pslice.bin_size(1), 1);
        assert_eq!(pslice.bin_size(2), 0);
    }

    #[test]
    fn test_pslice_iter_by_proximity() {
        let pslice = PSlice::new(TEST_MAX_PO);

        let peer0 = OverlayAddress::from([0x80; 32]);
        let peer1 = OverlayAddress::from([0x40; 32]);
        let peer2 = OverlayAddress::from([0x20; 32]);

        pslice.add(peer2, 2);
        pslice.add(peer0, 0);
        pslice.add(peer1, 1);

        let collected: Vec<_> = pslice.iter_by_proximity().collect();
        assert_eq!(collected.len(), 3);
        assert_eq!(collected[0].0, 0);
        assert_eq!(collected[1].0, 1);
        assert_eq!(collected[2].0, 2);
    }

    #[test]
    fn test_pslice_iter_by_proximity_desc() {
        let pslice = PSlice::new(TEST_MAX_PO);

        let peer0 = OverlayAddress::from([0x80; 32]);
        let peer1 = OverlayAddress::from([0x40; 32]);
        let peer2 = OverlayAddress::from([0x20; 32]);

        pslice.add(peer0, 0);
        pslice.add(peer1, 1);
        pslice.add(peer2, 2);

        let collected: Vec<_> = pslice.iter_by_proximity_desc().collect();
        assert_eq!(collected.len(), 3);
        assert_eq!(collected[0].0, 2);
        assert_eq!(collected[1].0, 1);
        assert_eq!(collected[2].0, 0);
    }

    #[test]
    fn test_pslice_bin_sizes() {
        let pslice = PSlice::new(TEST_MAX_PO);

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
        let pslice = PSlice::new(TEST_MAX_PO);

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
    fn test_pslice_cache_invalidation() {
        let pslice = PSlice::new(TEST_MAX_PO);

        let peer0 = OverlayAddress::from([0x80; 32]);
        let peer1 = OverlayAddress::from([0x40; 32]);

        pslice.add(peer0, 0);

        // First access builds the cache
        let list1: Vec<_> = pslice.iter_by_proximity().collect();
        assert_eq!(list1.len(), 1);

        // Add another peer - should invalidate cache
        pslice.add(peer1, 1);

        // Should see both peers now
        let list2: Vec<_> = pslice.iter_by_proximity().collect();
        assert_eq!(list2.len(), 2);

        // Remove a peer - should invalidate cache again
        pslice.remove(&peer0);

        let list3: Vec<_> = pslice.iter_by_proximity().collect();
        assert_eq!(list3.len(), 1);
        assert_eq!(list3[0].1, peer1);
    }

    #[test]
    fn test_pslice_cache_reuse() {
        let pslice = PSlice::new(TEST_MAX_PO);

        let peer0 = OverlayAddress::from([0x80; 32]);
        let peer1 = OverlayAddress::from([0x40; 32]);

        pslice.add(peer0, 0);
        pslice.add(peer1, 1);

        // Access multiple times without mutation - should reuse cache
        let gen_before = pslice.generation.load(Ordering::Relaxed);

        let _list1: Vec<_> = pslice.iter_by_proximity().collect();
        let _list2: Vec<_> = pslice.iter_by_proximity().collect();
        let _list3: Vec<_> = pslice.iter_by_proximity_desc().collect();

        let gen_after = pslice.generation.load(Ordering::Relaxed);

        // Generation should not change from reads
        assert_eq!(gen_before, gen_after);
    }
}
