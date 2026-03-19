//! Per-bin FIFO candidate queues with deduplication.

use std::collections::{HashSet, VecDeque};

use parking_lot::Mutex;
use vertex_swarm_primitives::OverlayAddress;

/// Per-bin FIFO queues for connection candidates with global dedup.
pub(crate) struct CandidateQueues {
    bins: Vec<Mutex<VecDeque<OverlayAddress>>>,
    /// Global dedup set across all bins.
    pending: Mutex<HashSet<OverlayAddress>>,
    max_per_bin: usize,
}

impl CandidateQueues {
    /// Create empty queues for `num_bins` bins.
    pub(super) fn new(num_bins: usize, max_per_bin: usize) -> Self {
        Self {
            bins: (0..num_bins).map(|_| Mutex::new(VecDeque::new())).collect(),
            pending: Mutex::new(HashSet::new()),
            max_per_bin,
        }
    }

    /// Enqueue a candidate in the given bin. Returns true if newly inserted.
    pub(super) fn push(&self, bin: u8, peer: OverlayAddress) -> bool {
        let mut pending = self.pending.lock();
        if !pending.insert(peer) {
            return false;
        }

        if let Some(queue) = self.bins.get(bin as usize) {
            let mut q = queue.lock();
            q.push_back(peer);
            // Cap per-bin: drop oldest if over limit
            while q.len() > self.max_per_bin {
                if let Some(evicted) = q.pop_front() {
                    pending.remove(&evicted);
                }
            }
        } else {
            // Invalid bin — remove from dedup set
            pending.remove(&peer);
            return false;
        }

        true
    }

    /// Drain all bins (highest PO first) into a single Vec.
    pub(super) fn drain_all(&self) -> Vec<OverlayAddress> {
        let mut pending = self.pending.lock();
        let mut result = Vec::new();

        // Drain highest bin first for priority
        for queue in self.bins.iter().rev() {
            let mut q = queue.lock();
            result.extend(q.drain(..));
        }

        pending.clear();
        result
    }

    /// Check if a peer is already queued (without cloning the set).
    #[allow(dead_code)]
    pub(super) fn contains(&self, peer: &OverlayAddress) -> bool {
        self.pending.lock().contains(peer)
    }

    /// Clone the dedup set for snapshot purposes.
    pub(super) fn snapshot_queued(&self) -> HashSet<OverlayAddress> {
        self.pending.lock().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nectar_primitives::SwarmAddress;

    #[test]
    fn test_push_and_drain() {
        let queues = CandidateQueues::new(32, 16);
        let peer = SwarmAddress::with_first_byte(0x80);

        assert!(queues.push(0, peer));
        // Dedup: second push returns false
        assert!(!queues.push(0, peer));

        let drained = queues.drain_all();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0], peer);

        // After drain, can push again
        assert!(queues.push(0, peer));
    }

    #[test]
    fn test_drain_highest_first() {
        let queues = CandidateQueues::new(32, 16);
        let lo = SwarmAddress::with_first_byte(0x80);
        let hi = SwarmAddress::with_first_byte(0x40);

        queues.push(0, lo);
        queues.push(1, hi);

        let drained = queues.drain_all();
        assert_eq!(drained.len(), 2);
        // Higher bin first
        assert_eq!(drained[0], hi);
        assert_eq!(drained[1], lo);
    }

    #[test]
    fn test_per_bin_cap() {
        let queues = CandidateQueues::new(32, 2);

        let p1 = SwarmAddress::with_first_byte(0x80);
        let p2 = SwarmAddress::with_first_byte(0x81);
        let p3 = SwarmAddress::with_first_byte(0x82);

        assert!(queues.push(0, p1));
        assert!(queues.push(0, p2));
        assert!(queues.push(0, p3));

        // p1 should have been evicted (cap=2)
        let queued = queues.snapshot_queued();
        assert_eq!(queued.len(), 2);
        assert!(!queued.contains(&p1));
        assert!(queued.contains(&p2));
        assert!(queued.contains(&p3));
    }

    #[test]
    fn test_invalid_bin() {
        let queues = CandidateQueues::new(4, 16);
        let peer = SwarmAddress::with_first_byte(0x80);

        // Bin 5 doesn't exist (only 0-3)
        assert!(!queues.push(5, peer));
        assert!(queues.snapshot_queued().is_empty());
    }
}
