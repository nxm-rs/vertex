//! Lock-free per-peer scoring state using atomics.
//!
//! This module provides `PeerScoreState`, an atomic state container for tracking
//! peer reputation without mutex contention. Multiple protocol handlers can
//! record events concurrently on the same peer.
//!
//! The design follows the bandwidth accounting pattern:
//! - Hot counters use atomics (connection counts, score deltas)
//! - Cold data (IP history) uses separate locked storage
//! - State is wrapped in `Arc` and shared via cheap clones

use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};

use vertex_swarm_primitives::OverlayAddress;

use super::ip::current_unix_timestamp;

/// Atomic ordering used for all score operations.
///
/// We use Relaxed ordering because:
/// - Score values don't need to synchronize other memory
/// - Eventual consistency is acceptable for reputation metrics
/// - Maximum performance for high-frequency recording
const ORDERING: Ordering = Ordering::Relaxed;

/// Lock-free per-peer scoring state.
///
/// All counters use atomic operations, allowing concurrent updates from
/// multiple protocol handlers without contention. The state is typically
/// wrapped in `Arc` and shared via `ScoreHandle`.
#[derive(Debug)]
pub struct PeerScoreState {
    peer: OverlayAddress,

    /// Current score scaled by 1000 (to avoid floating point atomics).
    /// Actual score = score_millis / 1000.0
    score_millis: AtomicI64,

    /// Unix timestamp of last update.
    last_updated_unix: AtomicU64,

    /// Number of successful connections.
    connection_successes: AtomicU32,
    /// Number of connection timeouts.
    connection_timeouts: AtomicU32,
    /// Number of connection refusals.
    connection_refusals: AtomicU32,
    /// Number of protocol errors.
    protocol_errors: AtomicU32,
    /// Number of handshake failures.
    handshake_failures: AtomicU32,
    /// Number of chunks successfully delivered.
    chunks_delivered: AtomicU32,
    /// Number of invalid chunks received.
    invalid_chunks: AtomicU32,

    /// Sum of latency samples in milliseconds (for computing average).
    latency_sum_ms: AtomicU64,
    /// Number of latency samples.
    latency_samples: AtomicU32,
}

impl PeerScoreState {
    /// Create a new peer score state.
    pub fn new(peer: OverlayAddress) -> Self {
        Self {
            peer,
            score_millis: AtomicI64::new(0),
            last_updated_unix: AtomicU64::new(current_unix_timestamp()),
            connection_successes: AtomicU32::new(0),
            connection_timeouts: AtomicU32::new(0),
            connection_refusals: AtomicU32::new(0),
            protocol_errors: AtomicU32::new(0),
            handshake_failures: AtomicU32::new(0),
            chunks_delivered: AtomicU32::new(0),
            invalid_chunks: AtomicU32::new(0),
            latency_sum_ms: AtomicU64::new(0),
            latency_samples: AtomicU32::new(0),
        }
    }

    /// Get the peer's overlay address.
    pub fn peer(&self) -> OverlayAddress {
        self.peer
    }

    /// Get the current score as a float.
    pub fn score(&self) -> f64 {
        self.score_millis.load(ORDERING) as f64 / 1000.0
    }

    /// Add to the score (can be negative for penalties).
    pub fn add_score(&self, delta: f64) {
        let delta_millis = (delta * 1000.0) as i64;
        self.score_millis.fetch_add(delta_millis, ORDERING);
        self.last_updated_unix
            .store(current_unix_timestamp(), ORDERING);
    }

    /// Get the last update timestamp.
    pub fn last_updated_unix(&self) -> u64 {
        self.last_updated_unix.load(ORDERING)
    }

    /// Record a successful connection.
    pub fn record_connection_success(&self) {
        self.connection_successes.fetch_add(1, ORDERING);
    }

    /// Record a connection timeout.
    pub fn record_connection_timeout(&self) {
        self.connection_timeouts.fetch_add(1, ORDERING);
    }

    /// Record a connection refusal.
    pub fn record_connection_refused(&self) {
        self.connection_refusals.fetch_add(1, ORDERING);
    }

    /// Record a protocol error.
    pub fn record_protocol_error(&self) {
        self.protocol_errors.fetch_add(1, ORDERING);
    }

    /// Record a handshake failure.
    pub fn record_handshake_failure(&self) {
        self.handshake_failures.fetch_add(1, ORDERING);
    }

    /// Record a successful chunk delivery.
    pub fn record_chunk_delivered(&self) {
        self.chunks_delivered.fetch_add(1, ORDERING);
    }

    /// Record an invalid chunk.
    pub fn record_invalid_chunk(&self) {
        self.invalid_chunks.fetch_add(1, ORDERING);
    }

    /// Record a latency sample.
    pub fn record_latency(&self, latency_ms: u32) {
        self.latency_sum_ms.fetch_add(latency_ms as u64, ORDERING);
        self.latency_samples.fetch_add(1, ORDERING);
    }

    /// Get connection success count.
    pub fn connection_successes(&self) -> u32 {
        self.connection_successes.load(ORDERING)
    }

    /// Get connection timeout count.
    pub fn connection_timeouts(&self) -> u32 {
        self.connection_timeouts.load(ORDERING)
    }

    /// Get connection refusal count.
    pub fn connection_refusals(&self) -> u32 {
        self.connection_refusals.load(ORDERING)
    }

    /// Get protocol error count.
    pub fn protocol_errors(&self) -> u32 {
        self.protocol_errors.load(ORDERING)
    }

    /// Get handshake failure count.
    pub fn handshake_failures(&self) -> u32 {
        self.handshake_failures.load(ORDERING)
    }

    /// Get chunk delivery count.
    pub fn chunks_delivered(&self) -> u32 {
        self.chunks_delivered.load(ORDERING)
    }

    /// Get invalid chunk count.
    pub fn invalid_chunks(&self) -> u32 {
        self.invalid_chunks.load(ORDERING)
    }

    /// Total connection attempts (success + all failure types).
    pub fn total_connection_attempts(&self) -> u32 {
        self.connection_successes()
            + self.connection_timeouts()
            + self.connection_refusals()
            + self.handshake_failures()
    }

    /// Calculate success rate as a value between 0.0 and 1.0.
    ///
    /// Returns 0.5 (neutral) if no connection attempts have been made.
    pub fn success_rate(&self) -> f64 {
        let total = self.total_connection_attempts();
        if total == 0 {
            return 0.5;
        }
        self.connection_successes() as f64 / total as f64
    }

    /// Get average latency in milliseconds.
    ///
    /// Returns `None` if no latency samples have been recorded.
    pub fn avg_latency_ms(&self) -> Option<u32> {
        let samples = self.latency_samples.load(ORDERING);
        if samples == 0 {
            return None;
        }
        let sum = self.latency_sum_ms.load(ORDERING);
        Some((sum / samples as u64) as u32)
    }

    /// Check if peer should be auto-banned based on score.
    pub fn should_ban(&self, threshold: f64) -> bool {
        self.score() < threshold
    }

    /// Check if peer should be deprioritized.
    pub fn should_deprioritize(&self, threshold: f64) -> bool {
        self.score() < threshold
    }

    /// Returns true if this peer has any recorded activity.
    pub fn has_activity(&self) -> bool {
        self.total_connection_attempts() > 0 || self.chunks_delivered() > 0
    }

    /// Create a snapshot for persistence.
    pub fn snapshot(&self) -> PeerScoreSnapshot {
        PeerScoreSnapshot {
            score: self.score(),
            last_updated_unix: self.last_updated_unix(),
            connection_successes: self.connection_successes(),
            connection_timeouts: self.connection_timeouts(),
            connection_refusals: self.connection_refusals(),
            protocol_errors: self.protocol_errors(),
            handshake_failures: self.handshake_failures(),
            chunks_delivered: self.chunks_delivered(),
            invalid_chunks: self.invalid_chunks(),
            latency_sum_ms: self.latency_sum_ms.load(ORDERING),
            latency_samples: self.latency_samples.load(ORDERING),
        }
    }

    /// Restore state from a snapshot.
    pub fn restore(&self, snapshot: &PeerScoreSnapshot) {
        self.score_millis
            .store((snapshot.score * 1000.0) as i64, ORDERING);
        self.last_updated_unix
            .store(snapshot.last_updated_unix, ORDERING);
        self.connection_successes
            .store(snapshot.connection_successes, ORDERING);
        self.connection_timeouts
            .store(snapshot.connection_timeouts, ORDERING);
        self.connection_refusals
            .store(snapshot.connection_refusals, ORDERING);
        self.protocol_errors
            .store(snapshot.protocol_errors, ORDERING);
        self.handshake_failures
            .store(snapshot.handshake_failures, ORDERING);
        self.chunks_delivered
            .store(snapshot.chunks_delivered, ORDERING);
        self.invalid_chunks.store(snapshot.invalid_chunks, ORDERING);
        self.latency_sum_ms.store(snapshot.latency_sum_ms, ORDERING);
        self.latency_samples
            .store(snapshot.latency_samples, ORDERING);
    }
}

/// Serializable snapshot of peer score state for persistence.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct PeerScoreSnapshot {
    pub score: f64,
    pub last_updated_unix: u64,
    pub connection_successes: u32,
    pub connection_timeouts: u32,
    pub connection_refusals: u32,
    pub protocol_errors: u32,
    pub handshake_failures: u32,
    pub chunks_delivered: u32,
    pub invalid_chunks: u32,
    pub latency_sum_ms: u64,
    pub latency_samples: u32,
}

impl PeerScoreSnapshot {
    /// Create a new empty score snapshot.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a successful connection.
    pub fn record_connection_success(&mut self) {
        self.connection_successes = self.connection_successes.saturating_add(1);
        self.last_updated_unix = super::ip::current_unix_timestamp();
    }

    /// Record a connection timeout.
    pub fn record_connection_timeout(&mut self) {
        self.connection_timeouts = self.connection_timeouts.saturating_add(1);
        self.last_updated_unix = super::ip::current_unix_timestamp();
    }

    /// Record a connection refusal.
    pub fn record_connection_refused(&mut self) {
        self.connection_refusals = self.connection_refusals.saturating_add(1);
        self.last_updated_unix = super::ip::current_unix_timestamp();
    }

    /// Record a handshake failure.
    pub fn record_handshake_failure(&mut self) {
        self.handshake_failures = self.handshake_failures.saturating_add(1);
        self.last_updated_unix = super::ip::current_unix_timestamp();
    }

    /// Record a protocol error.
    pub fn record_protocol_error(&mut self) {
        self.protocol_errors = self.protocol_errors.saturating_add(1);
        self.last_updated_unix = super::ip::current_unix_timestamp();
    }

    /// Total connection attempts.
    pub fn total_connection_attempts(&self) -> u32 {
        self.connection_successes
            + self.connection_timeouts
            + self.connection_refusals
            + self.handshake_failures
    }
}

impl PeerScoreSnapshot {
    /// Calculate success rate from snapshot data.
    pub fn success_rate(&self) -> f64 {
        let total = self.connection_successes
            + self.connection_timeouts
            + self.connection_refusals
            + self.handshake_failures;
        if total == 0 {
            return 0.5;
        }
        self.connection_successes as f64 / total as f64
    }

    /// Get average latency from snapshot data.
    pub fn avg_latency_ms(&self) -> Option<u32> {
        if self.latency_samples == 0 {
            return None;
        }
        Some((self.latency_sum_ms / self.latency_samples as u64) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;

    fn test_overlay(n: u8) -> OverlayAddress {
        OverlayAddress::from(B256::repeat_byte(n))
    }

    #[test]
    fn test_new_state() {
        let state = PeerScoreState::new(test_overlay(1));
        assert_eq!(state.score(), 0.0);
        assert_eq!(state.success_rate(), 0.5);
        assert!(!state.has_activity());
    }

    #[test]
    fn test_add_score() {
        let state = PeerScoreState::new(test_overlay(1));

        state.add_score(1.5);
        assert!((state.score() - 1.5).abs() < 0.01);

        state.add_score(-0.5);
        assert!((state.score() - 1.0).abs() < 0.01);

        state.add_score(-2.0);
        assert!((state.score() - (-1.0)).abs() < 0.01);
    }

    #[test]
    fn test_record_counters() {
        let state = PeerScoreState::new(test_overlay(1));

        state.record_connection_success();
        state.record_connection_success();
        state.record_connection_timeout();

        assert_eq!(state.connection_successes(), 2);
        assert_eq!(state.connection_timeouts(), 1);
        assert_eq!(state.total_connection_attempts(), 3);
        assert!(state.has_activity());
    }

    #[test]
    fn test_success_rate() {
        let state = PeerScoreState::new(test_overlay(1));

        for _ in 0..8 {
            state.record_connection_success();
        }
        for _ in 0..2 {
            state.record_connection_timeout();
        }

        assert!((state.success_rate() - 0.8).abs() < 0.01);
    }

    #[test]
    fn test_latency_tracking() {
        let state = PeerScoreState::new(test_overlay(1));

        assert!(state.avg_latency_ms().is_none());

        state.record_latency(100);
        assert_eq!(state.avg_latency_ms(), Some(100));

        state.record_latency(200);
        assert_eq!(state.avg_latency_ms(), Some(150));

        state.record_latency(300);
        assert_eq!(state.avg_latency_ms(), Some(200));
    }

    #[test]
    fn test_snapshot_restore() {
        let state = PeerScoreState::new(test_overlay(1));

        state.add_score(5.0);
        state.record_connection_success();
        state.record_connection_success();
        state.record_protocol_error();
        state.record_latency(150);

        let snapshot = state.snapshot();
        assert!((snapshot.score - 5.0).abs() < 0.01);
        assert_eq!(snapshot.connection_successes, 2);
        assert_eq!(snapshot.protocol_errors, 1);

        // Restore to a new state
        let state2 = PeerScoreState::new(test_overlay(1));
        state2.restore(&snapshot);

        assert!((state2.score() - 5.0).abs() < 0.01);
        assert_eq!(state2.connection_successes(), 2);
        assert_eq!(state2.protocol_errors(), 1);
        assert_eq!(state2.avg_latency_ms(), Some(150));
    }

    #[test]
    fn test_concurrent_updates() {
        use std::sync::Arc;
        use std::thread;

        let state = Arc::new(PeerScoreState::new(test_overlay(1)));
        let mut handles = vec![];

        // Spawn multiple threads updating the same state
        for _ in 0..10 {
            let state = Arc::clone(&state);
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    state.record_connection_success();
                    state.add_score(0.1);
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // All updates should be recorded
        assert_eq!(state.connection_successes(), 1000);
        assert!((state.score() - 100.0).abs() < 0.1);
    }

    #[test]
    fn test_ban_threshold() {
        let state = PeerScoreState::new(test_overlay(1));

        state.add_score(-50.0);
        assert!(state.should_ban(-40.0));
        assert!(!state.should_ban(-60.0));
    }
}
