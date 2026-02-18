//! Lock-free peer scoring with atomics.

use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering, fence};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::snapshot::PeerScoreSnapshot;
use crate::traits::NetPeerScoreExt;

/// Fixed-point multiplier for storing f64 scores as i64 atomics.
const SCORE_SCALE: f64 = 100_000.0;
/// Minimum allowed score (matches ban threshold scale).
const MIN_SCORE: f64 = -100.0;
/// Maximum allowed score (symmetric with minimum).
const MAX_SCORE: f64 = 100.0;

/// Lock-free peer scoring using atomics for concurrent access.
#[derive(Debug)]
pub struct PeerScore<Ext: NetPeerScoreExt = ()> {
    score: AtomicI64,
    last_updated: AtomicU64,
    connection_successes: AtomicU32,
    connection_timeouts: AtomicU32,
    connection_refusals: AtomicU32,
    handshake_failures: AtomicU32,
    protocol_errors: AtomicU32,
    latency_sum_nanos: AtomicU64,
    latency_samples: AtomicU32,
    ext: Ext,
}

impl<Ext: NetPeerScoreExt> Default for PeerScore<Ext> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Ext: NetPeerScoreExt> PeerScore<Ext> {
    pub fn new() -> Self {
        Self {
            score: AtomicI64::new(0),
            last_updated: AtomicU64::new(unix_timestamp_secs()),
            connection_successes: AtomicU32::new(0),
            connection_timeouts: AtomicU32::new(0),
            connection_refusals: AtomicU32::new(0),
            handshake_failures: AtomicU32::new(0),
            protocol_errors: AtomicU32::new(0),
            latency_sum_nanos: AtomicU64::new(0),
            latency_samples: AtomicU32::new(0),
            ext: Ext::default(),
        }
    }

    pub fn with_ext(ext: Ext) -> Self {
        Self { ext, ..Self::new() }
    }

    pub fn score(&self) -> f64 {
        self.score.load(Ordering::Acquire) as f64 / SCORE_SCALE
    }

    /// Atomically adjust score, clamped to bounds.
    pub fn add_score(&self, delta: f64) {
        let delta_scaled = (delta * SCORE_SCALE) as i64;
        loop {
            let current = self.score.load(Ordering::Acquire);
            let new_val = current.saturating_add(delta_scaled);
            let clamped = new_val.clamp(
                (MIN_SCORE * SCORE_SCALE) as i64,
                (MAX_SCORE * SCORE_SCALE) as i64,
            );
            if self
                .score
                .compare_exchange_weak(current, clamped, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                self.touch();
                break;
            }
        }
    }

    pub fn set_score(&self, score: f64) {
        let clamped = score.clamp(MIN_SCORE, MAX_SCORE);
        self.score.store((clamped * SCORE_SCALE) as i64, Ordering::Release);
        self.touch();
    }

    pub fn should_ban(&self, threshold: f64) -> bool {
        self.score() < threshold
    }

    pub fn last_updated(&self) -> u64 {
        self.last_updated.load(Ordering::Relaxed)
    }

    pub fn touch(&self) {
        self.last_updated.store(unix_timestamp_secs(), Ordering::Release);
    }

    pub fn connection_successes(&self) -> u32 {
        self.connection_successes.load(Ordering::Relaxed)
    }

    pub fn connection_timeouts(&self) -> u32 {
        self.connection_timeouts.load(Ordering::Relaxed)
    }

    pub fn connection_refusals(&self) -> u32 {
        self.connection_refusals.load(Ordering::Relaxed)
    }

    pub fn handshake_failures(&self) -> u32 {
        self.handshake_failures.load(Ordering::Relaxed)
    }

    pub fn protocol_errors(&self) -> u32 {
        self.protocol_errors.load(Ordering::Relaxed)
    }

    pub fn record_success(&self, latency_nanos: u64) {
        self.connection_successes.fetch_add(1, Ordering::Relaxed);
        self.record_latency(latency_nanos);
        self.touch();
    }

    pub fn record_timeout(&self) {
        self.connection_timeouts.fetch_add(1, Ordering::Relaxed);
        self.touch();
    }

    pub fn record_refusal(&self) {
        self.connection_refusals.fetch_add(1, Ordering::Relaxed);
        self.touch();
    }

    pub fn record_handshake_failure(&self) {
        self.handshake_failures.fetch_add(1, Ordering::Relaxed);
        self.touch();
    }

    pub fn record_protocol_error(&self) {
        self.protocol_errors.fetch_add(1, Ordering::Relaxed);
        self.touch();
    }

    pub fn total_connection_attempts(&self) -> u32 {
        self.connection_successes()
            + self.connection_timeouts()
            + self.connection_refusals()
            + self.handshake_failures()
    }

    /// Returns 0.5 (neutral) if no attempts recorded.
    pub fn success_rate(&self) -> f64 {
        let total = self.total_connection_attempts();
        if total == 0 {
            return 0.5;
        }
        self.connection_successes() as f64 / total as f64
    }

    pub fn record_latency(&self, latency_nanos: u64) {
        // Use Release on samples to synchronize with sum
        self.latency_sum_nanos.fetch_add(latency_nanos, Ordering::Relaxed);
        self.latency_samples.fetch_add(1, Ordering::Release);
    }

    pub fn latency_sum_nanos(&self) -> u64 {
        self.latency_sum_nanos.load(Ordering::Relaxed)
    }

    pub fn latency_samples(&self) -> u32 {
        self.latency_samples.load(Ordering::Relaxed)
    }

    /// Average latency in nanoseconds, or None if no samples recorded.
    pub fn avg_latency_nanos(&self) -> Option<u64> {
        // Acquire on samples synchronizes with Release in record_latency
        let samples = self.latency_samples.load(Ordering::Acquire);
        if samples == 0 {
            return None;
        }
        Some(self.latency_sum_nanos.load(Ordering::Relaxed) / samples as u64)
    }

    pub fn avg_latency(&self) -> Option<Duration> {
        self.avg_latency_nanos().map(Duration::from_nanos)
    }

    pub fn ext(&self) -> &Ext {
        &self.ext
    }

    /// Create a point-in-time snapshot for persistence.
    pub fn snapshot(&self) -> PeerScoreSnapshot<Ext::Snapshot> {
        // Acquire fence ensures we see all prior writes consistently
        fence(Ordering::Acquire);
        PeerScoreSnapshot::new(
            self.score.load(Ordering::Relaxed) as f64 / SCORE_SCALE,
            self.last_updated.load(Ordering::Relaxed),
            self.connection_successes.load(Ordering::Relaxed),
            self.connection_timeouts.load(Ordering::Relaxed),
            self.connection_refusals.load(Ordering::Relaxed),
            self.handshake_failures.load(Ordering::Relaxed),
            self.protocol_errors.load(Ordering::Relaxed),
            self.latency_sum_nanos.load(Ordering::Relaxed),
            self.latency_samples.load(Ordering::Relaxed),
            self.ext.snapshot(),
        )
    }

    /// Restore state from a snapshot.
    pub fn restore(&self, snapshot: &PeerScoreSnapshot<Ext::Snapshot>) {
        self.score.store((snapshot.score() * SCORE_SCALE) as i64, Ordering::Relaxed);
        self.last_updated.store(snapshot.last_updated(), Ordering::Relaxed);
        self.connection_successes.store(snapshot.connection_successes(), Ordering::Relaxed);
        self.connection_timeouts.store(snapshot.connection_timeouts(), Ordering::Relaxed);
        self.connection_refusals.store(snapshot.connection_refusals(), Ordering::Relaxed);
        self.handshake_failures.store(snapshot.handshake_failures(), Ordering::Relaxed);
        self.protocol_errors.store(snapshot.protocol_errors(), Ordering::Relaxed);
        self.latency_sum_nanos.store(snapshot.latency_sum_nanos(), Ordering::Relaxed);
        self.latency_samples.store(snapshot.latency_samples(), Ordering::Relaxed);
        self.ext.restore(snapshot.ext());
        // Release fence ensures all stores are visible to subsequent reads
        fence(Ordering::Release);
    }
}

fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_new_score() {
        let score: PeerScore = PeerScore::new();
        assert_eq!(score.score(), 0.0);
        assert_eq!(score.connection_successes(), 0);
        assert_eq!(score.success_rate(), 0.5);
    }

    #[test]
    fn test_add_score() {
        let score: PeerScore = PeerScore::new();

        score.add_score(10.0);
        assert!((score.score() - 10.0).abs() < 0.001);

        score.add_score(-5.0);
        assert!((score.score() - 5.0).abs() < 0.001);

        score.set_score(200.0);
        assert!((score.score() - MAX_SCORE).abs() < 0.001);

        score.set_score(-200.0);
        assert!((score.score() - MIN_SCORE).abs() < 0.001);
    }

    #[test]
    fn test_record_operations() {
        let score: PeerScore = PeerScore::new();

        score.record_success(50_000_000);
        assert_eq!(score.connection_successes(), 1);
        assert!(score.avg_latency_nanos().is_some());

        score.record_timeout();
        assert_eq!(score.connection_timeouts(), 1);

        score.record_refusal();
        assert_eq!(score.connection_refusals(), 1);

        score.record_handshake_failure();
        assert_eq!(score.handshake_failures(), 1);

        score.record_protocol_error();
        assert_eq!(score.protocol_errors(), 1);

        assert_eq!(score.total_connection_attempts(), 4);
    }

    #[test]
    fn test_success_rate() {
        let score: PeerScore = PeerScore::new();

        for _ in 0..8 {
            score.record_success(0);
        }
        for _ in 0..2 {
            score.record_timeout();
        }

        assert!((score.success_rate() - 0.8).abs() < 0.01);
    }

    #[test]
    fn test_latency_averaging() {
        let score: PeerScore = PeerScore::new();

        assert!(score.avg_latency_nanos().is_none());

        score.record_latency(100_000_000);
        assert_eq!(score.avg_latency_nanos(), Some(100_000_000));

        score.record_latency(200_000_000);
        assert_eq!(score.avg_latency_nanos(), Some(150_000_000));

        score.record_latency(300_000_000);
        assert_eq!(score.avg_latency_nanos(), Some(200_000_000));
    }

    #[test]
    fn test_snapshot_roundtrip() {
        let score: PeerScore = PeerScore::new();

        score.set_score(75.5);
        score.record_success(100_000_000);
        score.record_success(200_000_000);
        score.record_timeout();
        score.record_refusal();
        score.record_protocol_error();

        let snapshot = score.snapshot();
        assert!((snapshot.score() - 75.5).abs() < 0.01);
        assert_eq!(snapshot.connection_successes(), 2);
        assert_eq!(snapshot.connection_timeouts(), 1);
        assert_eq!(snapshot.connection_refusals(), 1);
        assert_eq!(snapshot.protocol_errors(), 1);

        let score2: PeerScore = PeerScore::new();
        score2.restore(&snapshot);

        assert!((score2.score() - 75.5).abs() < 0.01);
        assert_eq!(score2.connection_successes(), 2);
        assert_eq!(score2.connection_timeouts(), 1);
    }

    #[test]
    fn test_concurrent_updates() {
        let score: Arc<PeerScore> = Arc::new(PeerScore::new());
        let mut handles = vec![];

        for _ in 0..10 {
            let score = Arc::clone(&score);
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    score.add_score(1.0);
                    score.record_success(1_000_000);
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // Score clamps to MAX_SCORE (100.0), but all 1000 successes are counted
        assert!((score.score() - MAX_SCORE).abs() < 0.01);
        assert_eq!(score.connection_successes(), 1000);
    }

    #[test]
    fn test_should_ban() {
        let score: PeerScore = PeerScore::new();

        score.set_score(-50.0);
        assert!(score.should_ban(-40.0));
        assert!(!score.should_ban(-60.0));
    }
}
