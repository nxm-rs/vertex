//! Generic dial request tracker with bounded queue and in-flight management.

use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;
use std::time::Instant;

use hashlink::LinkedHashMap;
use libp2p::PeerId;

use crate::config::DialTrackerConfig;
use crate::types::{CleanupResult, DialCounts, DialDispatch, DialRequest, EnqueueResult};

/// In-flight dial entry.
struct InFlightEntry<Id, D> {
    request: DialRequest<Id, D>,
    started_at: Instant,
}

/// Generic dial request tracker.
///
/// Manages a bounded FIFO queue of pending dial requests and a set of in-flight
/// dials. Primary key is `PeerId` (always known). `Id` is an optional
/// application-level identifier that may be unknown at dial time and resolved
/// later (e.g., overlay address learned during handshake).
///
/// Key invariant: each `PeerId` appears at most once across pending + in-flight.
/// Each `Id` (when present) also appears at most once across pending + in-flight.
pub struct DialTracker<Id, D> {
    config: DialTrackerConfig,
    /// Pending requests in insertion order (FIFO), keyed by PeerId.
    pending: LinkedHashMap<PeerId, DialRequest<Id, D>>,
    /// In-flight dials keyed by PeerId.
    in_flight: HashMap<PeerId, InFlightEntry<Id, D>>,
    /// Reverse index: Id → PeerId for O(1) Id-based lookups.
    /// Covers both pending and in-flight entries that have a known Id.
    id_index: HashMap<Id, PeerId>,
    /// Timestamp of last cleanup run.
    last_cleanup: Instant,
}

impl<Id: Clone + Eq + Hash + Debug, D: Debug> DialTracker<Id, D> {
    pub fn new(config: DialTrackerConfig) -> Self {
        Self {
            config,
            pending: LinkedHashMap::new(),
            in_flight: HashMap::new(),
            id_index: HashMap::new(),
            last_cleanup: Instant::now(),
        }
    }

    /// Add a request to the pending queue.
    ///
    /// Deduplicates by PeerId and (if present) by Id.
    pub fn enqueue(&mut self, request: DialRequest<Id, D>) -> EnqueueResult {
        if self.pending.contains_key(&request.peer_id)
            || self.in_flight.contains_key(&request.peer_id)
        {
            if self.pending.contains_key(&request.peer_id) {
                return EnqueueResult::AlreadyPending;
            }
            return EnqueueResult::AlreadyInFlight;
        }
        // Check Id-based dedup
        if let Some(id) = &request.id {
            if self.id_index.contains_key(id) {
                // Id already tracked under a different PeerId
                return EnqueueResult::AlreadyPending;
            }
        }
        if self.pending.len() >= self.config.max_pending {
            return EnqueueResult::QueueFull;
        }

        let peer_id = request.peer_id;
        if let Some(id) = &request.id {
            self.id_index.insert(id.clone(), peer_id);
        }
        self.pending.insert(peer_id, request);
        EnqueueResult::Enqueued
    }

    /// Remove a specific pending request by PeerId.
    pub fn remove_pending(&mut self, peer_id: &PeerId) -> Option<DialRequest<Id, D>> {
        let request = self.pending.remove(peer_id)?;
        if let Some(id) = &request.id {
            self.id_index.remove(id);
        }
        Some(request)
    }

    /// Drain all pending requests (does NOT move to in-flight).
    pub fn drain_pending(&mut self) -> Vec<DialRequest<Id, D>> {
        let drained: Vec<_> = self.pending.drain().map(|(_, r)| r).collect();
        for request in &drained {
            if let Some(id) = &request.id {
                self.id_index.remove(id);
            }
        }
        drained
    }

    /// Get next pending dial, move to in-flight, return dispatch info.
    ///
    /// Skips expired entries. Respects `max_in_flight`.
    pub fn next_dial(&mut self) -> Option<DialDispatch<Id>> {
        self.next_batch(1).into_iter().next()
    }

    /// Get a batch of pending dials, move to in-flight.
    pub fn next_batch(&mut self, max: usize) -> Vec<DialDispatch<Id>> {
        // Periodic cleanup
        if self.last_cleanup.elapsed() > self.config.cleanup_interval {
            self.cleanup_expired();
            self.last_cleanup = Instant::now();
        }

        let available_slots = self.config.max_in_flight.saturating_sub(self.in_flight.len());
        let batch_size = max.min(available_slots);

        if batch_size == 0 {
            return Vec::new();
        }

        let mut result = Vec::with_capacity(batch_size);
        let mut expired = Vec::new();

        for (peer_id, request) in self.pending.iter() {
            if result.len() >= batch_size {
                break;
            }

            // Check TTL expiry
            if request.queued_at.elapsed() > self.config.pending_ttl {
                expired.push(*peer_id);
                continue;
            }

            result.push(DialDispatch {
                id: request.id.clone(),
                peer_id: *peer_id,
                addrs: request.addrs.clone(),
            });
        }

        // Clean up expired entries
        for peer_id in expired {
            if let Some(request) = self.pending.remove(&peer_id) {
                if let Some(id) = &request.id {
                    self.id_index.remove(id);
                }
            }
        }

        // Move dispatched entries from pending to in-flight
        let now = Instant::now();
        for dispatch in &result {
            if let Some(request) = self.pending.remove(&dispatch.peer_id) {
                self.in_flight.insert(
                    dispatch.peer_id,
                    InFlightEntry {
                        request,
                        started_at: now,
                    },
                );
                // id_index already points to this PeerId, no update needed
            }
        }

        result
    }

    /// Register a dial directly as in-flight (skip queue).
    ///
    /// Returns `Err(request)` if the PeerId or Id is already tracked.
    pub fn start_dial(&mut self, request: DialRequest<Id, D>) -> Result<(), DialRequest<Id, D>> {
        if self.pending.contains_key(&request.peer_id)
            || self.in_flight.contains_key(&request.peer_id)
        {
            return Err(request);
        }
        if let Some(id) = &request.id {
            if self.id_index.contains_key(id) {
                return Err(request);
            }
        }

        let peer_id = request.peer_id;
        if let Some(id) = &request.id {
            self.id_index.insert(id.clone(), peer_id);
        }
        self.in_flight.insert(
            peer_id,
            InFlightEntry {
                request,
                started_at: Instant::now(),
            },
        );
        Ok(())
    }

    /// Resolve an in-flight dial by PeerId. Returns the full original request.
    pub fn resolve(&mut self, peer_id: &PeerId) -> Option<DialRequest<Id, D>> {
        let entry = self.in_flight.remove(peer_id)?;
        if let Some(id) = &entry.request.id {
            self.id_index.remove(id);
        }
        Some(entry.request)
    }

    /// Check if a PeerId is pending or in-flight.
    pub fn contains_peer(&self, peer_id: &PeerId) -> bool {
        self.pending.contains_key(peer_id) || self.in_flight.contains_key(peer_id)
    }

    /// Check if an Id is pending or in-flight.
    pub fn contains_id(&self, id: &Id) -> bool {
        self.id_index.contains_key(id)
    }

    /// Check if PeerId is in the pending queue.
    pub fn is_pending(&self, peer_id: &PeerId) -> bool {
        self.pending.contains_key(peer_id)
    }

    /// Check if PeerId is in-flight.
    pub fn is_in_flight(&self, peer_id: &PeerId) -> bool {
        self.in_flight.contains_key(peer_id)
    }

    /// Current tracker counts.
    pub fn counts(&self) -> DialCounts {
        DialCounts {
            pending: self.pending.len(),
            in_flight: self.in_flight.len(),
        }
    }

    /// Clean up expired pending entries and timed-out in-flight entries.
    pub fn cleanup_expired(&mut self) -> CleanupResult<Id, D> {
        // Collect expired pending
        let expired_peer_ids: Vec<PeerId> = self
            .pending
            .iter()
            .filter(|(_, r)| r.queued_at.elapsed() > self.config.pending_ttl)
            .map(|(peer_id, _)| *peer_id)
            .collect();

        let mut expired_pending = Vec::with_capacity(expired_peer_ids.len());
        for peer_id in expired_peer_ids {
            if let Some(request) = self.pending.remove(&peer_id) {
                if let Some(id) = &request.id {
                    self.id_index.remove(id);
                }
                expired_pending.push(request);
            }
        }

        // Collect timed-out in-flight
        let timed_out_peer_ids: Vec<PeerId> = self
            .in_flight
            .iter()
            .filter(|(_, entry)| entry.started_at.elapsed() > self.config.in_flight_timeout)
            .map(|(peer_id, _)| *peer_id)
            .collect();

        let mut timed_out_in_flight = Vec::with_capacity(timed_out_peer_ids.len());
        for peer_id in timed_out_peer_ids {
            if let Some(entry) = self.in_flight.remove(&peer_id) {
                if let Some(id) = &entry.request.id {
                    self.id_index.remove(id);
                }
                timed_out_in_flight.push(entry.request);
            }
        }

        self.last_cleanup = Instant::now();

        CleanupResult {
            expired_pending,
            timed_out_in_flight,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    type TestId = u64;

    fn test_peer_id(index: u8) -> PeerId {
        let mut bytes = [0u8; 32];
        bytes[0] = index;
        let key = libp2p::identity::ed25519::SecretKey::try_from_bytes(bytes).unwrap();
        let keypair = libp2p::identity::ed25519::Keypair::from(key);
        PeerId::from_public_key(&libp2p::identity::PublicKey::from(keypair.public()))
    }

    fn test_addr(port: u16) -> libp2p::Multiaddr {
        format!("/ip4/127.0.0.1/tcp/{}", port).parse().unwrap()
    }

    fn make_request(id: TestId, peer_idx: u8) -> DialRequest<TestId, &'static str> {
        DialRequest::new(
            id,
            test_peer_id(peer_idx),
            vec![test_addr(9000 + id as u16)],
            "test-data",
        )
    }

    fn make_request_no_id(peer_idx: u8) -> DialRequest<TestId, &'static str> {
        DialRequest::without_id(
            test_peer_id(peer_idx),
            vec![test_addr(9000 + peer_idx as u16)],
            "test-data",
        )
    }

    fn test_config() -> DialTrackerConfig {
        DialTrackerConfig {
            max_pending: 10,
            max_in_flight: 3,
            pending_ttl: Duration::from_secs(60),
            in_flight_timeout: Duration::from_secs(60),
            cleanup_interval: Duration::from_secs(600),
        }
    }

    #[test]
    fn test_enqueue_and_counts() {
        let mut tracker = DialTracker::new(test_config());

        let result = tracker.enqueue(make_request(1, 1));
        assert_eq!(result, EnqueueResult::Enqueued);
        assert_eq!(
            tracker.counts(),
            DialCounts {
                pending: 1,
                in_flight: 0
            }
        );
        assert!(tracker.is_pending(&test_peer_id(1)));
        assert!(tracker.contains_peer(&test_peer_id(1)));
        assert!(tracker.contains_id(&1));
    }

    #[test]
    fn test_enqueue_dedup_by_peer_id() {
        let mut tracker = DialTracker::new(test_config());

        tracker.enqueue(make_request(1, 1));
        // Same PeerId, different Id
        let result = tracker.enqueue(make_request(2, 1));
        assert_eq!(result, EnqueueResult::AlreadyPending);
        assert_eq!(tracker.counts().pending, 1);
    }

    #[test]
    fn test_enqueue_dedup_by_id() {
        let mut tracker = DialTracker::new(test_config());

        tracker.enqueue(make_request(1, 1));
        // Same Id, different PeerId
        let result = tracker.enqueue(make_request(1, 2));
        assert_eq!(result, EnqueueResult::AlreadyPending);
        assert_eq!(tracker.counts().pending, 1);
    }

    #[test]
    fn test_enqueue_dedup_in_flight() {
        let mut tracker = DialTracker::new(test_config());

        tracker.enqueue(make_request(1, 1));
        tracker.next_dial();

        let result = tracker.enqueue(make_request(2, 1));
        assert_eq!(result, EnqueueResult::AlreadyInFlight);
    }

    #[test]
    fn test_enqueue_queue_full() {
        let mut config = test_config();
        config.max_pending = 2;
        let mut tracker = DialTracker::new(config);

        tracker.enqueue(make_request(1, 1));
        tracker.enqueue(make_request(2, 2));
        let result = tracker.enqueue(make_request(3, 3));
        assert_eq!(result, EnqueueResult::QueueFull);
    }

    #[test]
    fn test_next_dial_moves_to_in_flight() {
        let mut tracker = DialTracker::new(test_config());

        tracker.enqueue(make_request(1, 1));
        tracker.enqueue(make_request(2, 2));

        let dispatch = tracker.next_dial().unwrap();
        assert_eq!(dispatch.id, Some(1));
        assert_eq!(dispatch.peer_id, test_peer_id(1));

        assert!(!tracker.is_pending(&test_peer_id(1)));
        assert!(tracker.is_in_flight(&test_peer_id(1)));
        assert!(tracker.contains_id(&1));
        assert_eq!(
            tracker.counts(),
            DialCounts {
                pending: 1,
                in_flight: 1
            }
        );
    }

    #[test]
    fn test_next_dial_respects_max_in_flight() {
        let mut config = test_config();
        config.max_in_flight = 2;
        let mut tracker = DialTracker::new(config);

        for i in 0..5u8 {
            tracker.enqueue(make_request(i as u64, i + 10));
        }

        assert!(tracker.next_dial().is_some());
        assert!(tracker.next_dial().is_some());
        assert!(tracker.next_dial().is_none());

        assert_eq!(
            tracker.counts(),
            DialCounts {
                pending: 3,
                in_flight: 2
            }
        );
    }

    #[test]
    fn test_next_batch() {
        let mut tracker = DialTracker::new(test_config());

        for i in 0..5u8 {
            tracker.enqueue(make_request(i as u64, i + 10));
        }

        let batch = tracker.next_batch(3);
        assert_eq!(batch.len(), 3);
        assert_eq!(batch[0].id, Some(0));
        assert_eq!(batch[1].id, Some(1));
        assert_eq!(batch[2].id, Some(2));

        assert_eq!(
            tracker.counts(),
            DialCounts {
                pending: 2,
                in_flight: 3
            }
        );
    }

    #[test]
    fn test_resolve_returns_original_request() {
        let mut tracker = DialTracker::new(test_config());

        let peer_id = test_peer_id(1);
        tracker.enqueue(make_request(42, 1));
        tracker.next_dial();

        let resolved = tracker.resolve(&peer_id).unwrap();
        assert_eq!(resolved.id, Some(42));
        assert_eq!(resolved.data, "test-data");
        assert!(!tracker.is_in_flight(&peer_id));
        assert!(!tracker.contains_id(&42));
    }

    #[test]
    fn test_resolve_unknown_peer_id() {
        let mut tracker: DialTracker<TestId, &str> = DialTracker::new(test_config());
        assert!(tracker.resolve(&test_peer_id(99)).is_none());
    }

    #[test]
    fn test_start_dial_direct() {
        let mut tracker = DialTracker::new(test_config());

        let peer_id = test_peer_id(1);
        assert!(tracker.start_dial(make_request(1, 1)).is_ok());

        assert!(tracker.is_in_flight(&peer_id));
        assert!(tracker.contains_id(&1));
        assert_eq!(
            tracker.counts(),
            DialCounts {
                pending: 0,
                in_flight: 1
            }
        );
    }

    #[test]
    fn test_start_dial_rejects_duplicate_peer_id() {
        let mut tracker = DialTracker::new(test_config());

        tracker.start_dial(make_request(1, 1)).unwrap();
        let result = tracker.start_dial(make_request(2, 1));
        assert!(result.is_err());
    }

    #[test]
    fn test_start_dial_rejects_duplicate_id() {
        let mut tracker = DialTracker::new(test_config());

        tracker.start_dial(make_request(1, 1)).unwrap();
        let result = tracker.start_dial(make_request(1, 2));
        assert!(result.is_err());
    }

    #[test]
    fn test_start_dial_without_id() {
        let mut tracker = DialTracker::new(test_config());

        let peer_id = test_peer_id(1);
        assert!(tracker.start_dial(make_request_no_id(1)).is_ok());
        assert!(tracker.is_in_flight(&peer_id));
        assert_eq!(tracker.counts().in_flight, 1);

        assert!(tracker.start_dial(make_request_no_id(2)).is_ok());
        assert_eq!(tracker.counts().in_flight, 2);
    }

    #[test]
    fn test_remove_pending() {
        let mut tracker = DialTracker::new(test_config());

        tracker.enqueue(make_request(1, 1));
        let removed = tracker.remove_pending(&test_peer_id(1));
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().id, Some(1));
        assert_eq!(tracker.counts().pending, 0);
        assert!(!tracker.contains_id(&1));
    }

    #[test]
    fn test_drain_pending() {
        let mut tracker = DialTracker::new(test_config());

        tracker.enqueue(make_request(1, 1));
        tracker.enqueue(make_request(2, 2));
        tracker.start_dial(make_request(3, 3)).unwrap();

        let drained = tracker.drain_pending();
        assert_eq!(drained.len(), 2);
        assert!(!tracker.contains_id(&1));
        assert!(!tracker.contains_id(&2));
        assert!(tracker.contains_id(&3));
        assert_eq!(
            tracker.counts(),
            DialCounts {
                pending: 0,
                in_flight: 1
            }
        );
    }

    #[test]
    fn test_cleanup_expired_pending() {
        let mut config = test_config();
        config.pending_ttl = Duration::from_millis(0);
        let mut tracker = DialTracker::new(config);

        tracker.enqueue(make_request(1, 1));
        tracker.enqueue(make_request(2, 2));

        let result = tracker.cleanup_expired();
        assert_eq!(result.expired_pending.len(), 2);
        assert_eq!(tracker.counts().pending, 0);
        assert!(!tracker.contains_id(&1));
    }

    #[test]
    fn test_cleanup_timed_out_in_flight() {
        let mut config = test_config();
        config.in_flight_timeout = Duration::from_millis(0);
        let mut tracker = DialTracker::new(config);

        tracker.start_dial(make_request(1, 1)).unwrap();
        tracker.start_dial(make_request(2, 2)).unwrap();

        let result = tracker.cleanup_expired();
        assert_eq!(result.timed_out_in_flight.len(), 2);
        assert_eq!(tracker.counts().in_flight, 0);
        assert!(!tracker.contains_id(&1));
    }

    #[test]
    fn test_next_dial_skips_expired() {
        let mut config = test_config();
        config.pending_ttl = Duration::from_millis(0);
        config.cleanup_interval = Duration::from_millis(0);
        let mut tracker = DialTracker::new(config);

        tracker.enqueue(make_request(1, 1));

        assert!(tracker.next_dial().is_none());
        assert_eq!(tracker.counts().pending, 0);
    }

    #[test]
    fn test_fifo_ordering() {
        let mut config = test_config();
        config.max_in_flight = 10;
        let mut tracker = DialTracker::new(config);

        for i in 0..5u8 {
            tracker.enqueue(make_request(i as u64, i + 10));
        }

        for i in 0..5u64 {
            let dispatch = tracker.next_dial().unwrap();
            assert_eq!(dispatch.id, Some(i));
        }
    }

    #[test]
    fn test_resolve_frees_slot_for_next_dial() {
        let mut config = test_config();
        config.max_in_flight = 1;
        let mut tracker = DialTracker::new(config);

        let peer_id = test_peer_id(1);
        tracker.enqueue(make_request(1, 1));
        tracker.enqueue(make_request(2, 2));

        tracker.next_dial();
        assert!(tracker.next_dial().is_none());

        tracker.resolve(&peer_id);
        assert!(tracker.next_dial().is_some());
    }

    #[test]
    fn test_contains_checks_both() {
        let mut tracker = DialTracker::new(test_config());
        let peer_id = test_peer_id(1);

        tracker.enqueue(make_request(1, 1));
        assert!(tracker.contains_peer(&peer_id));
        assert!(tracker.contains_id(&1));

        tracker.next_dial();
        assert!(tracker.contains_peer(&peer_id));
        assert!(tracker.contains_id(&1));
        assert!(!tracker.is_pending(&peer_id));
        assert!(tracker.is_in_flight(&peer_id));
    }

    #[test]
    fn test_no_id_requests_dont_conflict() {
        let mut tracker = DialTracker::new(test_config());

        tracker.enqueue(make_request_no_id(1));
        let result = tracker.enqueue(make_request_no_id(2));
        assert_eq!(result, EnqueueResult::Enqueued);
        assert_eq!(tracker.counts().pending, 2);
    }

    #[test]
    fn test_cleanup_returns_full_requests() {
        let mut config = test_config();
        config.in_flight_timeout = Duration::from_millis(0);
        let mut tracker = DialTracker::new(config);

        tracker.start_dial(make_request(42, 1)).unwrap();

        let result = tracker.cleanup_expired();
        assert_eq!(result.timed_out_in_flight.len(), 1);
        assert_eq!(result.timed_out_in_flight[0].id, Some(42));
        assert_eq!(result.timed_out_in_flight[0].data, "test-data");
    }
}
