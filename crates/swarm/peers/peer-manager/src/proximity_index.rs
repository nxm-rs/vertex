//! Proximity-ordered storage for Kademlia-style routing.

use std::fmt;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use hashlink::LinkedHashSet;
use metrics::gauge;
use parking_lot::RwLock;
use vertex_swarm_primitives::OverlayAddress;

/// Error returned when adding a peer to the index fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddError {
    /// Peer already exists in the index (LRU position was touched).
    AlreadyPresent,
    /// Bin is at capacity; peer should be saved to DB only.
    BinFull,
}

impl fmt::Display for AddError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyPresent => write!(f, "peer already present in index"),
            Self::BinFull => write!(f, "bin at capacity"),
        }
    }
}

/// Proximity-ordered storage with per-bin locking and LRU ordering.
///
/// Stores overlay addresses by proximity order bins for Kademlia-style routing.
pub struct ProximityIndex {
    local_overlay: OverlayAddress,
    max_po: u8,
    /// 0 = unbounded.
    max_per_bin: usize,
    bins: Vec<RwLock<LinkedHashSet<OverlayAddress>>>,
    bin_counts: Vec<AtomicUsize>,
    total_count: AtomicUsize,
    generation: AtomicU64,
    cache: RwLock<CachedList>,
}

/// Cached sorted list with generation stamp.
struct CachedList {
    generation: Option<u64>,
    items: Arc<Vec<(u8, OverlayAddress)>>,
}

impl Default for CachedList {
    fn default() -> Self {
        Self {
            generation: None,
            items: Arc::new(Vec::new()),
        }
    }
}

impl ProximityIndex {
    /// Create a new index. Use `max_per_bin = 0` for unbounded storage.
    pub fn new(local_overlay: OverlayAddress, max_po: u8, max_per_bin: usize) -> Self {
        let num_bins = (max_po as usize) + 1;
        Self {
            local_overlay,
            max_po,
            max_per_bin,
            bins: (0..num_bins).map(|_| RwLock::new(LinkedHashSet::new())).collect(),
            bin_counts: (0..num_bins).map(|_| AtomicUsize::new(0)).collect(),
            total_count: AtomicUsize::new(0),
            generation: AtomicU64::new(0),
            cache: RwLock::new(CachedList::default()),
        }
    }

    #[must_use]
    pub fn local_overlay(&self) -> &OverlayAddress {
        &self.local_overlay
    }

    #[must_use]
    pub fn max_po(&self) -> u8 {
        self.max_po
    }

    #[must_use]
    pub fn max_per_bin(&self) -> usize {
        self.max_per_bin
    }

    #[must_use]
    pub fn bin_size(&self, po: u8) -> usize {
        self.bin_counts
            .get(po as usize)
            .map_or(0, |c| c.load(Ordering::Relaxed))
    }

    #[must_use]
    pub fn bin_sizes(&self) -> Vec<usize> {
        self.bin_counts
            .iter()
            .map(|c| c.load(Ordering::Relaxed))
            .collect()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.total_count.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.total_count.load(Ordering::Relaxed) == 0
    }

    /// Add an address to the index.
    ///
    /// Returns `Ok(())` if newly added. Returns `Err(AlreadyPresent)` if the
    /// address is already in the index. Returns `Err(BinFull)` if the bin is
    /// at capacity (peer should be saved to DB only).
    pub fn add(&self, addr: OverlayAddress) -> Result<(), AddError> {
        let po = self.bin_for(&addr);
        let mut bin = self.bins[po as usize].write();

        // Check duplicate first (before capacity check)
        if bin.contains(&addr) {
            return Err(AddError::AlreadyPresent);
        }

        // Enforce capacity limit
        if self.max_per_bin > 0 && bin.len() >= self.max_per_bin {
            return Err(AddError::BinFull);
        }

        bin.insert(addr);

        self.bin_counts[po as usize].fetch_add(1, Ordering::Relaxed);
        self.total_count.fetch_add(1, Ordering::Relaxed);
        let generation = self.generation.fetch_add(1, Ordering::Release) + 1;
        gauge!("topology_proximity_generation").set(generation as f64);
        Ok(())
    }

    /// Remove an address. Returns true if it existed.
    pub fn remove(&self, addr: &OverlayAddress) -> bool {
        let po = self.bin_for(addr);
        let mut bin = self.bins[po as usize].write();

        if !bin.remove(addr) {
            return false;
        }

        self.bin_counts[po as usize].fetch_sub(1, Ordering::Relaxed);
        self.total_count.fetch_sub(1, Ordering::Relaxed);
        let generation = self.generation.fetch_add(1, Ordering::Release) + 1;
        gauge!("topology_proximity_generation").set(generation as f64);
        true
    }

    /// Move an address to the back of its bin (most recently used).
    ///
    /// Returns true if the address existed and was moved.
    pub fn touch(&self, addr: &OverlayAddress) -> bool {
        let po = self.bin_for(addr);
        let mut bin = self.bins[po as usize].write();
        bin.to_back(addr)
    }

    /// Check if an address exists.
    #[must_use]
    pub fn exists(&self, addr: &OverlayAddress) -> bool {
        let po = self.bin_for(addr);
        self.bins[po as usize].read().contains(addr)
    }

    /// Get all addresses in a specific bin (LRU to MRU order).
    pub fn peers_in_bin(&self, po: u8) -> Vec<OverlayAddress> {
        self.bins
            .get(po as usize)
            .map_or_else(Vec::new, |bin| bin.read().iter().copied().collect())
    }

    /// Get up to `count` addresses from a bin that match `predicate` (LRU first).
    ///
    /// Iterates under the read lock with early exit once `count` matches are found,
    /// avoiding materializing the entire bin into a Vec.
    pub fn filter_bin(
        &self,
        po: u8,
        count: usize,
        mut predicate: impl FnMut(&OverlayAddress) -> bool,
    ) -> Vec<OverlayAddress> {
        let Some(bin) = self.bins.get(po as usize) else {
            return Vec::new();
        };
        let bin = bin.read();
        let mut result = Vec::with_capacity(count.min(bin.len()));
        for addr in bin.iter() {
            if result.len() >= count {
                break;
            }
            if predicate(addr) {
                result.push(*addr);
            }
        }
        result
    }

    /// Get up to `count` addresses from a bin (LRU first).
    pub fn take_lru_from_bin(&self, po: u8, count: usize) -> Vec<OverlayAddress> {
        self.bins
            .get(po as usize)
            .map_or_else(Vec::new, |bin| bin.read().iter().take(count).copied().collect())
    }

    /// Iterate from shallowest to deepest proximity order (ascending PO).
    pub fn iter_by_proximity(&self) -> impl ExactSizeIterator<Item = (u8, OverlayAddress)> {
        ArcIter::new(self.get_sorted())
    }

    /// Iterate from deepest to shallowest proximity order (descending PO).
    pub fn iter_by_proximity_desc(&self) -> impl ExactSizeIterator<Item = (u8, OverlayAddress)> {
        ArcIterRev::new(self.get_sorted())
    }

    /// Get all addresses as a flat list.
    pub fn all_peers(&self) -> Vec<OverlayAddress> {
        let mut result = Vec::with_capacity(self.len());
        for bin in &self.bins {
            result.extend(bin.read().iter().copied());
        }
        result
    }

    /// Compute bin (proximity order) for an address, capped at max_po.
    #[must_use]
    pub fn bin_for(&self, addr: &OverlayAddress) -> u8 {
        self.local_overlay.proximity(addr).min(self.max_po)
    }

    /// Build sorted list, using a generation-stamped cache to avoid rebuilds.
    ///
    /// Items are collected from bins *before* acquiring the cache write lock
    /// to avoid holding the cache lock while reading bins (which would block
    /// concurrent add/remove/touch operations).
    fn get_sorted(&self) -> Arc<Vec<(u8, OverlayAddress)>> {
        let current_gen = self.generation.load(Ordering::Acquire);

        // Fast path: cache is valid
        {
            let cache = self.cache.read();
            if cache.generation == Some(current_gen) {
                return Arc::clone(&cache.items);
            }
        }

        // Collect items WITHOUT holding the cache lock, so concurrent
        // add/remove/touch can proceed on bins without blocking.
        let mut items = Vec::with_capacity(self.len());
        for (po, bin) in self.bins.iter().enumerate() {
            for &addr in bin.read().iter() {
                items.push((po as u8, addr));
            }
        }

        // Now acquire cache write lock and store (or discard if another thread won).
        let mut cache = self.cache.write();
        let final_gen = self.generation.load(Ordering::Acquire);
        if cache.generation == Some(final_gen) {
            // Another thread rebuilt while we were collecting.
            return Arc::clone(&cache.items);
        }

        let items = Arc::new(items);
        gauge!("topology_proximity_cached_items").set(items.len() as f64);
        cache.items = Arc::clone(&items);
        cache.generation = Some(final_gen);
        items
    }
}

/// Forward iterator over Arc<Vec<T>> (yields copies, no heap allocation per item).
struct ArcIter<T: Copy> {
    data: Arc<Vec<T>>,
    index: usize,
}

impl<T: Copy> ArcIter<T> {
    fn new(data: Arc<Vec<T>>) -> Self {
        Self { data, index: 0 }
    }
}

impl<T: Copy> Iterator for ArcIter<T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index < self.data.len() {
            let item = self.data[self.index];
            self.index += 1;
            Some(item)
        } else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.data.len() - self.index;
        (remaining, Some(remaining))
    }
}

impl<T: Copy> ExactSizeIterator for ArcIter<T> {
    fn len(&self) -> usize {
        self.data.len() - self.index
    }
}

/// Reverse iterator over Arc<Vec<T>>.
struct ArcIterRev<T: Copy> {
    data: Arc<Vec<T>>,
    index: usize,
}

impl<T: Copy> ArcIterRev<T> {
    fn new(data: Arc<Vec<T>>) -> Self {
        Self {
            index: data.len(),
            data,
        }
    }
}

impl<T: Copy> Iterator for ArcIterRev<T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index > 0 {
            self.index -= 1;
            Some(self.data[self.index])
        } else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.index, Some(self.index))
    }
}

impl<T: Copy> ExactSizeIterator for ArcIterRev<T> {
    fn len(&self) -> usize {
        self.index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_overlay() -> OverlayAddress {
        OverlayAddress::from([0x00; 32])
    }

    fn overlay_in_bin(bin: u8) -> OverlayAddress {
        let mut bytes = [0x00u8; 32];
        if bin < 8 {
            bytes[0] = 0x80 >> bin;
        } else if bin < 16 {
            bytes[1] = 0x80 >> (bin - 8);
        } else if bin < 24 {
            bytes[2] = 0x80 >> (bin - 16);
        } else {
            bytes[3] = 0x80 >> (bin - 24);
        }
        OverlayAddress::from(bytes)
    }

    #[test]
    fn test_add_remove() {
        let index = ProximityIndex::new(local_overlay(), 31, 0);

        let addr = overlay_in_bin(0);
        assert!(index.add(addr).is_ok());
        assert_eq!(index.add(addr), Err(AddError::AlreadyPresent));
        assert!(index.exists(&addr));
        assert_eq!(index.len(), 1);

        assert!(index.remove(&addr));
        assert!(!index.remove(&addr)); // Already removed
        assert!(!index.exists(&addr));
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn test_bin_sizes() {
        let index = ProximityIndex::new(local_overlay(), 31, 0);

        let addr0 = OverlayAddress::from([0x80; 32]); // bin 0
        let addr1 = OverlayAddress::from([0x40; 32]); // bin 1
        let addr2 = OverlayAddress::from([0x20; 32]); // bin 2

        index.add(addr0).unwrap();
        index.add(addr1).unwrap();
        index.add(addr2).unwrap();

        assert_eq!(index.bin_size(0), 1);
        assert_eq!(index.bin_size(1), 1);
        assert_eq!(index.bin_size(2), 1);
        assert_eq!(index.len(), 3);
    }

    #[test]
    fn test_capacity_limit() {
        let index = ProximityIndex::new(local_overlay(), 31, 2);

        // All land in bin 0
        let addr1 = OverlayAddress::from([0x80; 32]);
        let addr2 = OverlayAddress::from([0xc0; 32]);
        let addr3 = OverlayAddress::from([0xa0; 32]);

        assert!(index.add(addr1).is_ok());
        assert!(index.add(addr2).is_ok());
        assert_eq!(index.bin_size(0), 2);

        // Third should fail (at capacity)
        assert_eq!(index.add(addr3), Err(AddError::BinFull));
        assert_eq!(index.bin_size(0), 2);
        assert!(!index.exists(&addr3));
    }

    #[test]
    fn test_iter_ascending() {
        let index = ProximityIndex::new(local_overlay(), 31, 0);

        let addr0 = OverlayAddress::from([0x80; 32]); // bin 0
        let addr1 = OverlayAddress::from([0x40; 32]); // bin 1
        let addr2 = OverlayAddress::from([0x20; 32]); // bin 2

        index.add(addr2).unwrap();
        index.add(addr0).unwrap();
        index.add(addr1).unwrap();

        let collected: Vec<_> = index.iter_by_proximity().collect();
        assert_eq!(collected.len(), 3);
        assert_eq!(collected[0].0, 0);
        assert_eq!(collected[1].0, 1);
        assert_eq!(collected[2].0, 2);
    }

    #[test]
    fn test_iter_descending() {
        let index = ProximityIndex::new(local_overlay(), 31, 0);

        let addr0 = OverlayAddress::from([0x80; 32]); // bin 0
        let addr1 = OverlayAddress::from([0x40; 32]); // bin 1
        let addr2 = OverlayAddress::from([0x20; 32]); // bin 2

        index.add(addr0).unwrap();
        index.add(addr1).unwrap();
        index.add(addr2).unwrap();

        let collected: Vec<_> = index.iter_by_proximity_desc().collect();
        assert_eq!(collected.len(), 3);
        assert_eq!(collected[0].0, 2);
        assert_eq!(collected[1].0, 1);
        assert_eq!(collected[2].0, 0);
    }

    #[test]
    fn test_cache_invalidation() {
        let index = ProximityIndex::new(local_overlay(), 31, 0);

        let addr0 = OverlayAddress::from([0x80; 32]);
        let addr1 = OverlayAddress::from([0x40; 32]);

        index.add(addr0).unwrap();
        let list1: Vec<_> = index.iter_by_proximity().collect();
        assert_eq!(list1.len(), 1);

        index.add(addr1).unwrap();
        let list2: Vec<_> = index.iter_by_proximity().collect();
        assert_eq!(list2.len(), 2);

        index.remove(&addr0);
        let list3: Vec<_> = index.iter_by_proximity().collect();
        assert_eq!(list3.len(), 1);
    }

    #[test]
    fn test_is_empty() {
        let index = ProximityIndex::new(local_overlay(), 31, 0);
        assert!(index.is_empty());

        index.add(OverlayAddress::from([0x80; 32])).unwrap();
        assert!(!index.is_empty());
    }

    #[test]
    fn test_peers_in_bin() {
        let index = ProximityIndex::new(local_overlay(), 31, 0);

        let addr1 = OverlayAddress::from([0x80; 32]); // bin 0
        let addr2 = OverlayAddress::from([0xc0; 32]); // bin 0
        let addr3 = OverlayAddress::from([0x40; 32]); // bin 1

        index.add(addr1).unwrap();
        index.add(addr2).unwrap();
        index.add(addr3).unwrap();

        let bin0 = index.peers_in_bin(0);
        assert_eq!(bin0.len(), 2);
        assert!(bin0.contains(&addr1));
        assert!(bin0.contains(&addr2));

        let bin1 = index.peers_in_bin(1);
        assert_eq!(bin1.len(), 1);
        assert!(bin1.contains(&addr3));
    }

    #[test]
    fn test_unbounded_capacity() {
        let index = ProximityIndex::new(local_overlay(), 31, 0);

        // Add many peers to same bin
        for i in 0..100 {
            let mut bytes = [0x80u8; 32];
            bytes[1] = i;
            let addr = OverlayAddress::from(bytes);
            assert!(index.add(addr).is_ok());
        }

        assert_eq!(index.bin_size(0), 100);
    }

    #[test]
    fn test_lru_insertion_order() {
        let index = ProximityIndex::new(local_overlay(), 31, 0);

        // All in bin 0
        let addr1 = OverlayAddress::from([0x80; 32]);
        let addr2 = OverlayAddress::from([0xc0; 32]);
        let addr3 = OverlayAddress::from([0xa0; 32]);

        index.add(addr1).unwrap();
        index.add(addr2).unwrap();
        index.add(addr3).unwrap();

        let peers = index.peers_in_bin(0);
        assert_eq!(peers.len(), 3);
        assert_eq!(peers[0], addr1); // First added = LRU
        assert_eq!(peers[1], addr2);
        assert_eq!(peers[2], addr3); // Last added = MRU
    }

    #[test]
    fn test_touch_moves_to_back() {
        let index = ProximityIndex::new(local_overlay(), 31, 0);

        let addr1 = OverlayAddress::from([0x80; 32]);
        let addr2 = OverlayAddress::from([0xc0; 32]);
        let addr3 = OverlayAddress::from([0xa0; 32]);

        index.add(addr1).unwrap();
        index.add(addr2).unwrap();
        index.add(addr3).unwrap();

        // Touch addr1 (move from front to back)
        assert!(index.touch(&addr1));

        let peers = index.peers_in_bin(0);
        assert_eq!(peers[0], addr2); // Now LRU
        assert_eq!(peers[1], addr3);
        assert_eq!(peers[2], addr1); // Now MRU
    }

    #[test]
    fn test_touch_nonexistent_returns_false() {
        let index = ProximityIndex::new(local_overlay(), 31, 0);

        let addr = OverlayAddress::from([0x80; 32]);
        assert!(!index.touch(&addr));
    }

    #[test]
    fn test_take_lru_from_bin() {
        let index = ProximityIndex::new(local_overlay(), 31, 0);

        let addr1 = OverlayAddress::from([0x80; 32]);
        let addr2 = OverlayAddress::from([0xc0; 32]);
        let addr3 = OverlayAddress::from([0xa0; 32]);

        index.add(addr1).unwrap();
        index.add(addr2).unwrap();
        index.add(addr3).unwrap();

        // Take 2 LRU peers
        let taken = index.take_lru_from_bin(0, 2);
        assert_eq!(taken.len(), 2);
        assert_eq!(taken[0], addr1);
        assert_eq!(taken[1], addr2);

        // Take more than available
        let taken_all = index.take_lru_from_bin(0, 10);
        assert_eq!(taken_all.len(), 3);

        // Take 0
        let taken_none = index.take_lru_from_bin(0, 0);
        assert!(taken_none.is_empty());
    }

    #[test]
    fn test_total_count_tracking() {
        let index = ProximityIndex::new(local_overlay(), 31, 0);

        let addr0 = OverlayAddress::from([0x80; 32]);
        let addr1 = OverlayAddress::from([0x40; 32]);

        assert_eq!(index.len(), 0);
        index.add(addr0).unwrap();
        assert_eq!(index.len(), 1);
        index.add(addr1).unwrap();
        assert_eq!(index.len(), 2);
        index.remove(&addr0);
        assert_eq!(index.len(), 1);
        index.remove(&addr1);
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn test_exact_size_iterator() {
        let index = ProximityIndex::new(local_overlay(), 31, 0);

        index.add(OverlayAddress::from([0x80; 32])).unwrap();
        index.add(OverlayAddress::from([0x40; 32])).unwrap();
        index.add(OverlayAddress::from([0x20; 32])).unwrap();

        let mut iter = index.iter_by_proximity();
        assert_eq!(iter.len(), 3);
        iter.next();
        assert_eq!(iter.len(), 2);

        let mut rev_iter = index.iter_by_proximity_desc();
        assert_eq!(rev_iter.len(), 3);
        rev_iter.next();
        assert_eq!(rev_iter.len(), 2);
    }

    #[test]
    fn test_filter_bin() {
        let index = ProximityIndex::new(local_overlay(), 31, 0);

        // All in bin 0
        let addr1 = OverlayAddress::from([0x80; 32]);
        let addr2 = OverlayAddress::from([0xc0; 32]);
        let addr3 = OverlayAddress::from([0xa0; 32]);
        let addr4 = OverlayAddress::from([0xb0; 32]);

        index.add(addr1).unwrap();
        index.add(addr2).unwrap();
        index.add(addr3).unwrap();
        index.add(addr4).unwrap();

        // Filter with predicate that rejects addr2
        let result = index.filter_bin(0, 3, |a| *a != addr2);
        assert_eq!(result.len(), 3);
        assert!(!result.contains(&addr2));

        // Filter with count limit (early exit)
        let result = index.filter_bin(0, 1, |_| true);
        assert_eq!(result.len(), 1);

        // Filter with no matches
        let result = index.filter_bin(0, 10, |_| false);
        assert!(result.is_empty());

        // Filter on empty bin
        let result = index.filter_bin(5, 10, |_| true);
        assert!(result.is_empty());

        // Filter on out-of-range bin
        let result = index.filter_bin(255, 10, |_| true);
        assert!(result.is_empty());
    }
}
