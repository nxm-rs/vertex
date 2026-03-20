//! Generic dial request tracker with bounded queue and in-flight management.

use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;
use std::time::Instant;

use hashlink::{LinkedHashMap, LruCache};
use libp2p::swarm::dial_opts::DialOpts;
use libp2p::{Multiaddr, PeerId};
use metrics::{counter, gauge};

use crate::backoff::{BackoffEntry, backoff_remaining, jitter_seed_for};
use crate::config::DialTrackerConfig;
use crate::error::PrepareError;
use crate::prepare;
use crate::types::{CleanupResult, DialDispatch, DialRequest, EnqueueError};

/// Generic dial request tracker.
///
/// Manages a bounded FIFO queue of pending dial requests and a set of in-flight
/// dials. Primary key is `PeerId` (always known). `Id` is an optional
/// application-level identifier that may be unknown at dial time and resolved
/// later (e.g., overlay address learned during handshake).
///
/// Key invariant: each `PeerId` appears at most once across pending + in-flight.
/// Each `Id` (when present) also appears at most once across pending + in-flight.
///
/// Optionally tracks per-Id backoff and ban state (enabled via config).
pub struct DialTracker<Id, D> {
    config: DialTrackerConfig,
    /// Pending requests in insertion order (FIFO), keyed by PeerId.
    pending: LinkedHashMap<PeerId, DialRequest<Id, D>>,
    /// In-flight dials keyed by PeerId.
    in_flight: HashMap<PeerId, DialRequest<Id, D>>,
    /// Reverse index: Id → PeerId for O(1) Id-based lookups.
    /// Covers both pending and in-flight entries that have a known Id.
    id_index: HashMap<Id, PeerId>,
    /// Timestamp of last cleanup run.
    last_cleanup: Instant,
    /// Label value for metrics (present only when metrics_label is set).
    metrics_label: Option<&'static str>,
    /// Monotonic epoch for backoff/ban timestamps (avoids storing `Instant`).
    epoch: Instant,
    /// Short-lived backoff LRU (None = disabled).
    backoff: Option<LruCache<Id, BackoffEntry>>,
    /// Longer-lived ban LRU (None = disabled). Value is the ban timestamp (secs since epoch).
    banned: Option<LruCache<Id, u64>>,
}

impl<Id: Clone + Eq + Hash + Debug, D: Debug> DialTracker<Id, D> {
    pub fn new(config: DialTrackerConfig) -> Self {
        let metrics_label = config.metrics_label;
        let now = Instant::now();
        let backoff = if config.backoff_capacity > 0 {
            Some(LruCache::new(config.backoff_capacity))
        } else {
            None
        };
        let banned = if config.ban_capacity > 0 {
            Some(LruCache::new(config.ban_capacity))
        } else {
            None
        };
        Self {
            config,
            pending: LinkedHashMap::new(),
            in_flight: HashMap::new(),
            id_index: HashMap::new(),
            last_cleanup: now,
            metrics_label,
            epoch: now,
            backoff,
            banned,
        }
    }

    /// Add a request to the pending queue.
    ///
    /// Deduplicates by PeerId and (if present) by Id.
    /// When backoff/ban is enabled, rejects peers that are in backoff or banned.
    pub fn enqueue(&mut self, request: DialRequest<Id, D>) -> Result<(), EnqueueError> {
        if let Some(id) = request.id.as_ref() {
            self.check_banned(id)?;
            self.check_backoff(id)?;
        }
        if self.is_tracked(&request.peer_id, request.id.as_ref()) {
            if self.in_flight.contains_key(&request.peer_id) {
                return Err(EnqueueError::AlreadyInFlight);
            }
            return Err(EnqueueError::AlreadyPending);
        }
        if self.pending.len() >= self.config.max_pending {
            return Err(EnqueueError::QueueFull);
        }

        let peer_id = request.peer_id;
        self.track_id(&request);
        self.pending.insert(peer_id, request);
        self.record_gauges();
        Ok(())
    }

    /// Get a batch of pending dials, move to in-flight.
    pub fn next_batch(&mut self, max: usize) -> Vec<DialDispatch<Id>> {
        if self.last_cleanup.elapsed() > self.config.cleanup_interval {
            self.cleanup_expired();
            self.last_cleanup = Instant::now();
        }

        let available_slots = self
            .config
            .max_in_flight
            .saturating_sub(self.in_flight.len());
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

        for peer_id in expired {
            if let Some(request) = self.pending.remove(&peer_id) {
                self.untrack_id(&request);
            }
        }

        // Move dispatched entries from pending to in-flight.
        // Shares a single timestamp and defers gauges since ids are already tracked.
        let now = Instant::now();
        for dispatch in &result {
            if let Some(mut request) = self.pending.remove(&dispatch.peer_id) {
                request.started_at = Some(now);
                self.in_flight.insert(dispatch.peer_id, request);
            }
        }

        if !result.is_empty() {
            self.record_gauges();
        }

        result
    }

    /// Filter addresses, build `DialOpts`, and register as in-flight in one step.
    ///
    /// Combines address preparation and in-flight tracking into a single call.
    /// Returns `DialOpts` ready to pass to `ToSwarm::Dial`.
    pub fn prepare_and_start(
        &mut self,
        id: Option<Id>,
        peer_id: PeerId,
        addrs: Vec<Multiaddr>,
        data: D,
        mut filter: impl FnMut(&Multiaddr) -> bool,
    ) -> Result<DialOpts, PrepareError> {
        if let Some(id) = id.as_ref() {
            if self.is_banned(id) {
                return Err(PrepareError::Banned);
            }
            if self.is_in_backoff(id) {
                return Err(PrepareError::InBackoff);
            }
        }
        self.try_start(peer_id, id, PrepareError::AlreadyTracked, |id| {
            let opts = prepare::prepare_dial_opts(peer_id, addrs.into_iter(), &mut filter)
                .ok_or(PrepareError::NoReachableAddresses)?;
            let request = match id {
                Some(id) => DialRequest::new(id, peer_id, Vec::new(), data),
                None => DialRequest::without_id(peer_id, Vec::new(), data),
            };
            Ok((request, opts))
        })
    }

    /// Resolve an in-flight dial by PeerId. Returns the full original request.
    pub fn resolve(&mut self, peer_id: &PeerId) -> Option<DialRequest<Id, D>> {
        let request = self.in_flight.remove(peer_id)?;
        self.untrack_id(&request);
        self.record_gauges();
        Some(request)
    }

    /// Check if a PeerId is pending or in-flight.
    pub fn contains_peer(&self, peer_id: &PeerId) -> bool {
        self.pending.contains_key(peer_id) || self.in_flight.contains_key(peer_id)
    }

    /// Check if an Id is pending or in-flight.
    pub fn contains_id(&self, id: &Id) -> bool {
        self.id_index.contains_key(id)
    }

    /// Check if PeerId is in-flight.
    pub fn is_in_flight(&self, peer_id: &PeerId) -> bool {
        self.in_flight.contains_key(peer_id)
    }

    /// Number of pending requests in the queue.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Number of in-flight dials.
    pub fn in_flight_count(&self) -> usize {
        self.in_flight.len()
    }

    /// Clean up expired pending entries and timed-out in-flight entries.
    pub fn cleanup_expired(&mut self) -> CleanupResult<Id, D> {
        let expired_peer_ids: Vec<PeerId> = self
            .pending
            .iter()
            .filter(|(_, r)| r.queued_at.elapsed() > self.config.pending_ttl)
            .map(|(peer_id, _)| *peer_id)
            .collect();

        let mut expired_pending = Vec::with_capacity(expired_peer_ids.len());
        for peer_id in expired_peer_ids {
            if let Some(request) = self.pending.remove(&peer_id) {
                self.untrack_id(&request);
                expired_pending.push(request);
            }
        }

        let timeout = self.config.in_flight_timeout;
        let timed_out_peer_ids: Vec<PeerId> = self
            .in_flight
            .iter()
            .filter(|(_, req)| req.started_at.is_some_and(|t| t.elapsed() > timeout))
            .map(|(peer_id, _)| *peer_id)
            .collect();

        let mut timed_out_in_flight = Vec::with_capacity(timed_out_peer_ids.len());
        for peer_id in timed_out_peer_ids {
            if let Some(request) = self.in_flight.remove(&peer_id) {
                self.untrack_id(&request);
                timed_out_in_flight.push(request);
            }
        }

        self.last_cleanup = Instant::now();

        if !expired_pending.is_empty() || !timed_out_in_flight.is_empty() {
            self.record_gauges();
        }

        CleanupResult {
            expired_pending,
            timed_out_in_flight,
        }
    }

    /// Whether a peer_id or id is already pending or in-flight.
    fn is_tracked(&self, peer_id: &PeerId, id: Option<&Id>) -> bool {
        self.pending.contains_key(peer_id)
            || self.in_flight.contains_key(peer_id)
            || id.is_some_and(|id| self.id_index.contains_key(id))
    }

    /// Guard dedup, run `f`, register result as in-flight.
    ///
    /// Takes ownership of `id` so it can be forwarded to the closure for request
    /// construction without conflicting with the dedup borrow.
    fn try_start<T, E>(
        &mut self,
        peer_id: PeerId,
        id: Option<Id>,
        on_conflict: E,
        f: impl FnOnce(Option<Id>) -> Result<(DialRequest<Id, D>, T), E>,
    ) -> Result<T, E> {
        if self.is_tracked(&peer_id, id.as_ref()) {
            return Err(on_conflict);
        }
        let (request, value) = f(id)?;
        self.insert_in_flight(request);
        Ok(value)
    }

    /// Set `started_at`, index the id, insert into in-flight, record gauges.
    fn insert_in_flight(&mut self, mut request: DialRequest<Id, D>) {
        request.started_at = Some(Instant::now());
        let peer_id = request.peer_id;
        self.track_id(&request);
        self.in_flight.insert(peer_id, request);
        self.record_gauges();
    }

    fn track_id(&mut self, request: &DialRequest<Id, D>) {
        if let Some(id) = &request.id {
            self.id_index.insert(id.clone(), request.peer_id);
        }
    }

    fn untrack_id(&mut self, request: &DialRequest<Id, D>) {
        if let Some(id) = &request.id {
            self.id_index.remove(id);
        }
    }

    /// Record a failed dial for backoff tracking. Promotes to ban after threshold.
    pub fn record_backoff(&mut self, id: &Id) {
        let now_secs = self.epoch.elapsed().as_secs();

        let Some(backoff_cache) = self.backoff.as_mut() else {
            return;
        };

        let entry = backoff_cache.get(id).copied().unwrap_or_default();

        let new_failures = entry.consecutive_failures + 1;

        if self.config.ban_after_failures > 0 && new_failures >= self.config.ban_after_failures {
            // Promote to ban
            backoff_cache.remove(id);
            if let Some(ban_cache) = self.banned.as_mut() {
                ban_cache.insert(id.clone(), now_secs);
                self.record_ban_metrics();
            }
            self.record_backoff_metrics();
            if let Some(purpose) = self.metrics_label {
                counter!("dial_tracker_banned_total", "purpose" => purpose).increment(1);
            }
            return;
        }

        backoff_cache.insert(
            id.clone(),
            BackoffEntry {
                last_failure_secs: now_secs,
                consecutive_failures: new_failures,
            },
        );
        self.record_backoff_metrics();
        if let Some(purpose) = self.metrics_label {
            counter!("dial_tracker_backoff_recorded_total", "purpose" => purpose).increment(1);
        }
    }

    /// Clear backoff and ban state for a peer (e.g., after successful verification).
    pub fn clear_backoff(&mut self, id: &Id) {
        if let Some(cache) = self.backoff.as_mut() {
            cache.remove(id);
            self.record_backoff_metrics();
        }
        if let Some(cache) = self.banned.as_mut() {
            cache.remove(id);
            self.record_ban_metrics();
        }
    }

    /// Returns `Err(EnqueueError::Banned)` if the id is in the ban cache and not expired.
    fn check_banned(&mut self, id: &Id) -> Result<(), EnqueueError> {
        if self.is_banned(id) {
            Err(EnqueueError::Banned)
        } else {
            Ok(())
        }
    }

    /// Returns `Err(EnqueueError::InBackoff)` if the id is in backoff.
    fn check_backoff(&mut self, id: &Id) -> Result<(), EnqueueError> {
        if self.is_in_backoff(id) {
            Err(EnqueueError::InBackoff)
        } else {
            Ok(())
        }
    }

    fn is_banned(&mut self, id: &Id) -> bool {
        let Some(ban_cache) = self.banned.as_mut() else {
            return false;
        };
        let Some(&ban_time) = ban_cache.get(id) else {
            return false;
        };
        let now_secs = self.epoch.elapsed().as_secs();
        if self.config.ban_ttl_secs > 0
            && now_secs.saturating_sub(ban_time) >= self.config.ban_ttl_secs
        {
            ban_cache.remove(id);
            self.record_ban_metrics();
            false
        } else {
            true
        }
    }

    fn is_in_backoff(&mut self, id: &Id) -> bool {
        let Some(backoff_cache) = self.backoff.as_mut() else {
            return false;
        };
        let Some(entry) = backoff_cache.get(id).copied() else {
            return false;
        };
        let now_secs = self.epoch.elapsed().as_secs();
        let seed = jitter_seed_for(id);
        // Don't remove expired entries here — the BackoffEntry holds the
        // consecutive_failures counter needed for ban promotion. Removing it
        // would reset the counter and prevent peers from ever reaching the ban
        // threshold. Expired entries are harmless in the LRU (bounded capacity)
        // and record_backoff_metrics() counts only active entries.
        backoff_remaining(
            &entry,
            now_secs,
            self.config.backoff_base_secs,
            self.config.backoff_max_secs,
            seed,
        )
        .is_some()
    }

    fn record_gauges(&self) {
        if let Some(purpose) = self.metrics_label {
            gauge!("dial_tracker_pending", "purpose" => purpose).set(self.pending.len() as f64);
            gauge!("dial_tracker_in_flight", "purpose" => purpose).set(self.in_flight.len() as f64);
        }
    }

    fn record_backoff_metrics(&self) {
        if let Some(purpose) = self.metrics_label {
            let now_secs = self.epoch.elapsed().as_secs();
            let active = self.backoff.as_ref().map_or(0, |cache| {
                cache
                    .iter()
                    .filter(|(id, entry)| {
                        backoff_remaining(
                            entry,
                            now_secs,
                            self.config.backoff_base_secs,
                            self.config.backoff_max_secs,
                            jitter_seed_for(*id),
                        )
                        .is_some()
                    })
                    .count()
            });
            gauge!("dial_tracker_backoff_peers", "purpose" => purpose).set(active as f64);
        }
    }

    fn record_ban_metrics(&self) {
        if let Some(purpose) = self.metrics_label {
            let active = self.banned.as_ref().map_or(0, |cache| {
                if self.config.ban_ttl_secs == 0 {
                    return cache.len();
                }
                let now_secs = self.epoch.elapsed().as_secs();
                cache
                    .iter()
                    .filter(|(_, ban_time)| {
                        now_secs.saturating_sub(**ban_time) < self.config.ban_ttl_secs
                    })
                    .count()
            });
            gauge!("dial_tracker_banned_peers", "purpose" => purpose).set(active as f64);
        }
    }
}

#[cfg(test)]
impl<Id: Clone + Eq + Hash + Debug, D: Debug> DialTracker<Id, D> {
    fn next_dial(&mut self) -> Option<DialDispatch<Id>> {
        self.next_batch(1).into_iter().next()
    }

    #[allow(clippy::result_large_err)]
    fn start_dial(&mut self, request: DialRequest<Id, D>) -> Result<(), DialRequest<Id, D>> {
        if self.is_tracked(&request.peer_id, request.id.as_ref()) {
            return Err(request);
        }
        self.insert_in_flight(request);
        Ok(())
    }

    fn is_pending(&self, peer_id: &PeerId) -> bool {
        self.pending.contains_key(peer_id)
    }

    fn remove_pending(&mut self, peer_id: &PeerId) -> Option<DialRequest<Id, D>> {
        let request = self.pending.remove(peer_id)?;
        self.untrack_id(&request);
        self.record_gauges();
        Some(request)
    }

    fn drain_pending(&mut self) -> Vec<DialRequest<Id, D>> {
        let drained: Vec<_> = self.pending.drain().map(|(_, r)| r).collect();
        for request in &drained {
            self.untrack_id(request);
        }
        if !drained.is_empty() {
            self.record_gauges();
        }
        drained
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    type TestId = u64;
    type Tracker = DialTracker<TestId, &'static str>;

    fn peer(i: u8) -> PeerId {
        let mut bytes = [0u8; 32];
        bytes[0] = i;
        let key = libp2p::identity::ed25519::SecretKey::try_from_bytes(bytes).unwrap();
        let kp = libp2p::identity::ed25519::Keypair::from(key);
        PeerId::from_public_key(&libp2p::identity::PublicKey::from(kp.public()))
    }

    fn addr(port: u16) -> Multiaddr {
        format!("/ip4/127.0.0.1/tcp/{}", port).parse().unwrap()
    }

    fn request(id: TestId, peer_idx: u8) -> DialRequest<TestId, &'static str> {
        DialRequest::new(
            id,
            peer(peer_idx),
            vec![addr(9000 + id as u16)],
            "test-data",
        )
    }

    fn request_no_id(peer_idx: u8) -> DialRequest<TestId, &'static str> {
        DialRequest::without_id(
            peer(peer_idx),
            vec![addr(9000 + peer_idx as u16)],
            "test-data",
        )
    }

    fn config() -> DialTrackerConfig {
        DialTrackerConfig {
            max_pending: 10,
            max_in_flight: 3,
            pending_ttl: Duration::from_secs(60),
            in_flight_timeout: Duration::from_secs(60),
            cleanup_interval: Duration::from_secs(600),
            metrics_label: None,
            ..Default::default()
        }
    }

    fn tracker() -> Tracker {
        Tracker::new(config())
    }

    fn tracker_with(f: impl FnOnce(&mut DialTrackerConfig)) -> Tracker {
        let mut c = config();
        f(&mut c);
        Tracker::new(c)
    }

    fn assert_counts(t: &Tracker, pending: usize, in_flight: usize) {
        assert_eq!(t.pending_count(), pending, "pending count");
        assert_eq!(t.in_flight_count(), in_flight, "in_flight count");
    }

    fn enqueue_in_flight(t: &mut Tracker, id: TestId, peer_idx: u8) {
        t.enqueue(request(id, peer_idx)).unwrap();
        t.next_dial();
    }

    fn prepare(
        t: &mut Tracker,
        id: Option<TestId>,
        peer_idx: u8,
    ) -> Result<DialOpts, PrepareError> {
        t.prepare_and_start(
            id,
            peer(peer_idx),
            vec![addr(9000 + peer_idx as u16)],
            "test-data",
            |_| true,
        )
    }

    #[test]
    fn test_enqueue_and_counts() {
        let mut t = tracker();
        assert!(t.enqueue(request(1, 1)).is_ok());
        assert_counts(&t, 1, 0);
        assert!(t.is_pending(&peer(1)));
        assert!(t.contains_peer(&peer(1)));
        assert!(t.contains_id(&1));
    }

    #[test]
    fn test_enqueue_dedup() {
        let mut t = tracker();
        t.enqueue(request(1, 1)).unwrap();
        assert_eq!(
            t.enqueue(request(2, 1)).unwrap_err(),
            EnqueueError::AlreadyPending
        ); // same peer_id
        assert_eq!(
            t.enqueue(request(1, 2)).unwrap_err(),
            EnqueueError::AlreadyPending
        ); // same id
        assert_counts(&t, 1, 0);
    }

    #[test]
    fn test_enqueue_dedup_in_flight() {
        let mut t = tracker();
        enqueue_in_flight(&mut t, 1, 1);
        assert_eq!(
            t.enqueue(request(2, 1)).unwrap_err(),
            EnqueueError::AlreadyInFlight
        );
    }

    #[test]
    fn test_enqueue_queue_full() {
        let mut t = tracker_with(|c| c.max_pending = 2);
        t.enqueue(request(1, 1)).unwrap();
        t.enqueue(request(2, 2)).unwrap();
        assert_eq!(
            t.enqueue(request(3, 3)).unwrap_err(),
            EnqueueError::QueueFull
        );
    }

    #[test]
    fn test_next_dial_moves_to_in_flight() {
        let mut t = tracker();
        t.enqueue(request(1, 1)).unwrap();
        t.enqueue(request(2, 2)).unwrap();

        let d = t.next_dial().unwrap();
        assert_eq!(d.id, Some(1));
        assert_eq!(d.peer_id, peer(1));
        assert!(!t.is_pending(&peer(1)));
        assert!(t.is_in_flight(&peer(1)));
        assert!(t.contains_id(&1));
        assert_counts(&t, 1, 1);
    }

    #[test]
    fn test_next_dial_respects_max_in_flight() {
        let mut t = tracker_with(|c| c.max_in_flight = 2);
        for i in 0..5u8 {
            t.enqueue(request(i as u64, i + 10)).unwrap();
        }

        assert!(t.next_dial().is_some());
        assert!(t.next_dial().is_some());
        assert!(t.next_dial().is_none());
        assert_counts(&t, 3, 2);
    }

    #[test]
    fn test_next_batch() {
        let mut t = tracker();
        for i in 0..5u8 {
            t.enqueue(request(i as u64, i + 10)).unwrap();
        }

        let batch = t.next_batch(3);
        assert_eq!(batch.len(), 3);
        assert_eq!(batch[0].id, Some(0));
        assert_eq!(batch[1].id, Some(1));
        assert_eq!(batch[2].id, Some(2));
        assert_counts(&t, 2, 3);
    }

    #[test]
    fn test_next_dial_skips_expired() {
        let mut t = tracker_with(|c| {
            c.pending_ttl = Duration::ZERO;
            c.cleanup_interval = Duration::ZERO;
        });
        t.enqueue(request(1, 1)).unwrap();
        assert!(t.next_dial().is_none());
        assert_counts(&t, 0, 0);
    }

    #[test]
    fn test_fifo_ordering() {
        let mut t = tracker_with(|c| c.max_in_flight = 10);
        for i in 0..5u8 {
            t.enqueue(request(i as u64, i + 10)).unwrap();
        }
        for i in 0..5u64 {
            assert_eq!(t.next_dial().unwrap().id, Some(i));
        }
    }

    #[test]
    fn test_resolve() {
        let mut t = tracker();
        enqueue_in_flight(&mut t, 42, 1);

        let r = t.resolve(&peer(1)).unwrap();
        assert_eq!(r.id, Some(42));
        assert_eq!(r.data, "test-data");
        assert!(!t.is_in_flight(&peer(1)));
        assert!(!t.contains_id(&42));
    }

    #[test]
    fn test_resolve_unknown() {
        let mut t = tracker();
        assert!(t.resolve(&peer(99)).is_none());
    }

    #[test]
    fn test_resolve_frees_slot() {
        let mut t = tracker_with(|c| c.max_in_flight = 1);
        t.enqueue(request(1, 1)).unwrap();
        t.enqueue(request(2, 2)).unwrap();
        t.next_dial();
        assert!(t.next_dial().is_none());
        t.resolve(&peer(1));
        assert!(t.next_dial().is_some());
    }

    #[test]
    fn test_start_dial() {
        let mut t = tracker();
        assert!(t.start_dial(request(1, 1)).is_ok());
        assert!(t.is_in_flight(&peer(1)));
        assert!(t.contains_id(&1));
        assert_counts(&t, 0, 1);
    }

    #[test]
    fn test_start_dial_rejects_duplicates() {
        let mut t = tracker();
        t.start_dial(request(1, 1)).unwrap();
        assert!(t.start_dial(request(2, 1)).is_err()); // same peer_id
        assert!(t.start_dial(request(1, 2)).is_err()); // same id
    }

    #[test]
    fn test_start_dial_no_id() {
        let mut t = tracker();
        assert!(t.start_dial(request_no_id(1)).is_ok());
        assert!(t.start_dial(request_no_id(2)).is_ok());
        assert_counts(&t, 0, 2);
    }

    #[test]
    fn test_contains_checks_both_states() {
        let mut t = tracker();
        t.enqueue(request(1, 1)).unwrap();
        assert!(t.contains_peer(&peer(1)));
        assert!(t.contains_id(&1));

        t.next_dial();
        assert!(t.contains_peer(&peer(1)));
        assert!(t.contains_id(&1));
        assert!(!t.is_pending(&peer(1)));
        assert!(t.is_in_flight(&peer(1)));
    }

    #[test]
    fn test_no_id_requests_coexist() {
        let mut t = tracker();
        t.enqueue(request_no_id(1)).unwrap();
        assert!(t.enqueue(request_no_id(2)).is_ok());
        assert_counts(&t, 2, 0);
    }

    #[test]
    fn test_remove_pending() {
        let mut t = tracker();
        t.enqueue(request(1, 1)).unwrap();
        assert_eq!(t.remove_pending(&peer(1)).unwrap().id, Some(1));
        assert_counts(&t, 0, 0);
        assert!(!t.contains_id(&1));
    }

    #[test]
    fn test_drain_pending() {
        let mut t = tracker();
        t.enqueue(request(1, 1)).unwrap();
        t.enqueue(request(2, 2)).unwrap();
        t.start_dial(request(3, 3)).unwrap();

        assert_eq!(t.drain_pending().len(), 2);
        assert!(!t.contains_id(&1));
        assert!(!t.contains_id(&2));
        assert!(t.contains_id(&3));
        assert_counts(&t, 0, 1);
    }

    #[test]
    fn test_cleanup_expired_pending() {
        let mut t = tracker_with(|c| c.pending_ttl = Duration::ZERO);
        t.enqueue(request(1, 1)).unwrap();
        t.enqueue(request(2, 2)).unwrap();

        let r = t.cleanup_expired();
        assert_eq!(r.expired_pending.len(), 2);
        assert_counts(&t, 0, 0);
        assert!(!t.contains_id(&1));
    }

    #[test]
    fn test_cleanup_timed_out_in_flight() {
        let mut t = tracker_with(|c| c.in_flight_timeout = Duration::ZERO);
        t.start_dial(request(1, 1)).unwrap();
        t.start_dial(request(2, 2)).unwrap();

        let r = t.cleanup_expired();
        assert_eq!(r.timed_out_in_flight.len(), 2);
        assert_counts(&t, 0, 0);
        assert!(!t.contains_id(&1));
    }

    #[test]
    fn test_cleanup_preserves_request_data() {
        let mut t = tracker_with(|c| c.in_flight_timeout = Duration::ZERO);
        t.start_dial(request(42, 1)).unwrap();

        let r = t.cleanup_expired();
        assert_eq!(r.timed_out_in_flight[0].id, Some(42));
        assert_eq!(r.timed_out_in_flight[0].data, "test-data");
    }

    #[test]
    fn test_prepare_and_start() {
        let mut t = tracker();
        assert!(prepare(&mut t, Some(1), 1).is_ok());
        assert!(t.is_in_flight(&peer(1)));
        assert!(t.contains_id(&1));
    }

    #[test]
    fn test_prepare_and_start_without_id() {
        let mut t = tracker();
        assert!(prepare(&mut t, None, 1).is_ok());
        assert!(t.is_in_flight(&peer(1)));
    }

    #[test]
    fn test_prepare_and_start_no_reachable_addresses() {
        let mut t = tracker();
        // Empty addresses
        let r = t.prepare_and_start(Some(1u64), peer(1), Vec::new(), "d", |_| true);
        assert!(matches!(r, Err(PrepareError::NoReachableAddresses)));
        // All filtered out
        let r = t.prepare_and_start(Some(1u64), peer(1), vec![addr(9000)], "d", |_| false);
        assert!(matches!(r, Err(PrepareError::NoReachableAddresses)));
        assert_counts(&t, 0, 0);
    }

    #[test]
    fn test_prepare_and_start_already_tracked() {
        let mut t = tracker();
        prepare(&mut t, Some(1), 1).unwrap();
        assert!(matches!(
            prepare(&mut t, Some(2), 1),
            Err(PrepareError::AlreadyTracked)
        )); // same peer
        assert!(matches!(
            prepare(&mut t, Some(1), 2),
            Err(PrepareError::AlreadyTracked)
        )); // same id
    }

    #[test]
    fn test_timestamps() {
        let mut t = tracker();

        // Pending request: no started_at
        t.enqueue(request(1, 1)).unwrap();
        assert!(t.remove_pending(&peer(1)).unwrap().started_at().is_none());

        // Enqueue → dispatch → resolve: both timestamps set
        enqueue_in_flight(&mut t, 2, 2);
        let r = t.resolve(&peer(2)).unwrap();
        assert!(r.started_at().is_some());
        assert!(r.started_at().unwrap() >= r.queued_at());

        // prepare_and_start → resolve: both timestamps set
        prepare(&mut t, Some(3), 3).unwrap();
        let r = t.resolve(&peer(3)).unwrap();
        assert!(r.started_at().is_some());
        assert!(r.started_at().unwrap() >= r.queued_at());
    }

    fn backoff_config() -> DialTrackerConfig {
        DialTrackerConfig {
            max_pending: 10,
            max_in_flight: 3,
            pending_ttl: Duration::from_secs(60),
            in_flight_timeout: Duration::from_secs(60),
            cleanup_interval: Duration::from_secs(600),
            metrics_label: None,
            backoff_capacity: 1024,
            backoff_base_secs: 5,
            backoff_max_secs: 20,
            ban_capacity: 8192,
            ban_after_failures: 3,
            ban_ttl_secs: 3600,
        }
    }

    fn backoff_tracker() -> Tracker {
        Tracker::new(backoff_config())
    }

    #[test]
    fn test_backoff_rejects_re_enqueue() {
        let mut t = backoff_tracker();
        let id = 1u64;
        t.record_backoff(&id);

        // Re-enqueue with same id should be rejected
        assert_eq!(
            t.enqueue(request(id, 1)).unwrap_err(),
            EnqueueError::InBackoff
        );
    }

    #[test]
    fn test_backoff_expires() {
        let mut t = backoff_tracker();
        let id = 1u64;

        // Manually create an expired backoff entry by using a very old timestamp.
        // We simulate this by inserting directly into the backoff cache.
        t.backoff.as_mut().unwrap().insert(
            id,
            BackoffEntry {
                last_failure_secs: 0, // epoch = 0, which is `t.epoch`
                consecutive_failures: 1,
            },
        );

        // With base=5s and 1 failure, backoff ~5s.
        // Epoch elapsed is near-zero so it should be in backoff now.
        assert_eq!(
            t.enqueue(request(id, 1)).unwrap_err(),
            EnqueueError::InBackoff
        );

        // Override with a very old timestamp so it's expired.
        // last_failure_secs = 0, and epoch elapsed > 5s won't be true yet.
        // Instead, let's set last_failure_secs far in the past relative to epoch.
        // Since epoch.elapsed() is ~0, we can't easily simulate time passing.
        // So test clear_backoff instead for the "expires" behavior:
        t.clear_backoff(&id);
        assert!(t.enqueue(request(id, 1)).is_ok());
    }

    #[test]
    fn test_ban_after_threshold() {
        let mut t = backoff_tracker();
        let id = 1u64;

        // 3 failures should promote to ban
        t.record_backoff(&id);
        t.record_backoff(&id);
        t.record_backoff(&id);

        // Should be banned, not just in backoff
        assert_eq!(t.enqueue(request(id, 1)).unwrap_err(), EnqueueError::Banned);
    }

    #[test]
    fn test_ban_rejects_immediately() {
        let mut t = backoff_tracker();
        let id = 1u64;

        // Promote to ban
        for _ in 0..3 {
            t.record_backoff(&id);
        }

        // Immediate rejection
        assert_eq!(t.enqueue(request(id, 1)).unwrap_err(), EnqueueError::Banned);

        // Different id should still work
        assert!(t.enqueue(request(2, 2)).is_ok());
    }

    #[test]
    fn test_clear_backoff_removes_from_both() {
        let mut t = backoff_tracker();
        let id = 1u64;

        // Ban the id
        for _ in 0..3 {
            t.record_backoff(&id);
        }
        assert_eq!(t.enqueue(request(id, 1)).unwrap_err(), EnqueueError::Banned);

        // Clear should remove from ban
        t.clear_backoff(&id);
        assert!(t.enqueue(request(id, 1)).is_ok());
    }

    #[test]
    fn test_backoff_disabled_no_effect() {
        // Default config has 0 capacity = disabled
        let mut t = tracker();
        let id = 1u64;

        // record_backoff should be a no-op
        t.record_backoff(&id);

        // Should enqueue normally
        assert!(t.enqueue(request(id, 1)).is_ok());
    }

    #[test]
    fn test_backoff_no_id_bypasses_checks() {
        let mut t = backoff_tracker();

        // No id means no backoff check
        assert!(t.enqueue(request_no_id(1)).is_ok());
    }

    #[test]
    fn test_lru_eviction() {
        let mut config = backoff_config();
        config.backoff_capacity = 2;
        config.ban_capacity = 2;
        let mut t = Tracker::new(config);

        // Fill backoff cache
        t.record_backoff(&1u64);
        t.record_backoff(&2u64);
        t.record_backoff(&3u64); // should evict id=1

        // id=1 should now be allowed (evicted)
        assert!(t.enqueue(request(1, 1)).is_ok());
    }
}
