//! Thread-safe buffer for batching StoredPeer writes before DB flush.

use std::collections::HashMap;

use parking_lot::Mutex;
use vertex_net_peer_store::NetPeerStore;
use vertex_swarm_peer_score::PeerScore;
use vertex_swarm_primitives::OverlayAddress;

use crate::entry::StoredPeer;

/// Batches `StoredPeer` and score writes for amortized DB flush.
///
/// Deduplicates by overlay address (latest write wins). O(1) push, batch flush.
pub(crate) struct WriteBuffer {
    pending: Mutex<HashMap<OverlayAddress, StoredPeer>>,
    pending_scores: Mutex<HashMap<OverlayAddress, PeerScore>>,
    capacity: usize,
}

impl WriteBuffer {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            pending: Mutex::new(HashMap::with_capacity(capacity)),
            pending_scores: Mutex::new(HashMap::with_capacity(capacity)),
            capacity,
        }
    }

    /// Insert or update a record. Returns true if buffer is at capacity.
    pub(crate) fn push(&self, record: StoredPeer) -> bool {
        let overlay = OverlayAddress::from(*record.peer.overlay());
        let mut pending = self.pending.lock();
        pending.insert(overlay, record);
        pending.len() >= self.capacity
    }

    /// Insert or update a score snapshot.
    pub(crate) fn push_score(&self, overlay: OverlayAddress, score: PeerScore) {
        self.pending_scores.lock().insert(overlay, score);
    }

    /// Take all pending records, leaving the buffer empty.
    pub(crate) fn drain(&self) -> Vec<StoredPeer> {
        let mut pending = self.pending.lock();
        pending.drain().map(|(_, v)| v).collect()
    }

    /// Take all pending score snapshots, leaving the buffer empty.
    pub(crate) fn drain_scores(&self) -> Vec<(OverlayAddress, PeerScore)> {
        let mut pending = self.pending_scores.lock();
        pending.drain().collect()
    }

    /// Drain pending records and save them to the store.
    pub(crate) fn flush(
        &self,
        store: &dyn NetPeerStore<StoredPeer>,
    ) -> Result<(), vertex_net_peer_store::error::StoreError> {
        let records = self.drain();
        if records.is_empty() {
            return Ok(());
        }
        store.save_batch(&records)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.pending.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_net_peer_store::MemoryPeerStore;
    use vertex_swarm_primitives::SwarmNodeType;
    use vertex_swarm_test_utils::test_swarm_peer;

    fn make_record(n: u8) -> StoredPeer {
        StoredPeer {
            peer: test_swarm_peer(n),
            node_type: SwarmNodeType::Client,
            ban_info: None,
            first_seen: 1000,
            last_seen: 2000,
            last_dial_attempt: 0,
            consecutive_failures: 0,
        }
    }

    #[test]
    fn test_push_and_drain() {
        let buf = WriteBuffer::new(64);
        assert_eq!(buf.len(), 0);

        buf.push(make_record(1));
        buf.push(make_record(2));
        assert_eq!(buf.len(), 2);

        let drained = buf.drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn test_dedup_by_overlay() {
        let buf = WriteBuffer::new(64);

        let mut r1 = make_record(1);
        r1.consecutive_failures = 0;
        buf.push(r1);

        // Push again with updated state — should overwrite
        let mut r1_updated = make_record(1);
        r1_updated.consecutive_failures = 5;
        buf.push(r1_updated);

        assert_eq!(buf.len(), 1);
        let drained = buf.drain();
        assert_eq!(drained[0].consecutive_failures, 5);
    }

    #[test]
    fn test_capacity_trigger() {
        let buf = WriteBuffer::new(3);

        assert!(!buf.push(make_record(1)));
        assert!(!buf.push(make_record(2)));
        assert!(buf.push(make_record(3))); // at capacity
    }

    #[test]
    fn test_flush_roundtrip() {
        let buf = WriteBuffer::new(64);
        let store = MemoryPeerStore::<StoredPeer>::new();

        buf.push(make_record(1));
        buf.push(make_record(2));
        buf.push(make_record(3));

        buf.flush(&store).unwrap();
        assert_eq!(buf.len(), 0);
        assert_eq!(store.count().unwrap(), 3);
    }

    #[test]
    fn test_flush_empty_is_noop() {
        let buf = WriteBuffer::new(64);
        let store = MemoryPeerStore::<StoredPeer>::new();
        buf.flush(&store).unwrap();
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn test_concurrent_access() {
        use std::sync::Arc;
        use std::thread;

        let buf = Arc::new(WriteBuffer::new(1000));
        let mut handles = vec![];

        for batch in 0..4 {
            let buf = Arc::clone(&buf);
            handles.push(thread::spawn(move || {
                for i in 0..25 {
                    buf.push(make_record((batch * 25 + i) as u8));
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(buf.len(), 100);
    }
}
