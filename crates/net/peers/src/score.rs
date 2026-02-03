//! Lock-free peer scoring with atomics.

use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::traits::NetPeerScoreExt;

/// Fixed-point scaling for score precision without floats in atomics.
const SCORE_SCALE: f64 = 100_000.0;
const MIN_SCORE: f64 = -1_000_000.0;
const MAX_SCORE: f64 = 1_000_000.0;
const ORD: Ordering = Ordering::Relaxed;

/// Lock-free peer scoring using atomics for concurrent access.
#[derive(Debug)]
pub struct PeerScore<Ext: NetPeerScoreExt = ()> {
    /// Fixed-point score (score * SCORE_SCALE).
    score: AtomicI64,
    last_updated: AtomicU64,
    connection_successes: AtomicU32,
    connection_timeouts: AtomicU32,
    connection_refusals: AtomicU32,
    handshake_failures: AtomicU32,
    protocol_errors: AtomicU32,
    latency_sum_nanos: AtomicU64,
    latency_samples: AtomicU32,
    /// Protocol-specific scoring extension.
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
            last_updated: AtomicU64::new(current_unix_timestamp()),
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
        self.score.load(ORD) as f64 / SCORE_SCALE
    }

    /// Atomically adjust score with CAS loop, clamped to bounds.
    pub fn add_score(&self, delta: f64) {
        let delta_scaled = (delta * SCORE_SCALE) as i64;
        loop {
            let current = self.score.load(ORD);
            let new_val = current.saturating_add(delta_scaled);
            let clamped = new_val.clamp(
                (MIN_SCORE * SCORE_SCALE) as i64,
                (MAX_SCORE * SCORE_SCALE) as i64,
            );
            if self
                .score
                .compare_exchange_weak(current, clamped, ORD, ORD)
                .is_ok()
            {
                self.touch();
                break;
            }
        }
    }

    pub fn set_score(&self, score: f64) {
        let clamped = score.clamp(MIN_SCORE, MAX_SCORE);
        self.score.store((clamped * SCORE_SCALE) as i64, ORD);
        self.touch();
    }

    pub fn should_ban(&self, threshold: f64) -> bool {
        self.score() < threshold
    }

    pub fn last_updated(&self) -> u64 {
        self.last_updated.load(ORD)
    }

    pub fn touch(&self) {
        self.last_updated.store(current_unix_timestamp(), ORD);
    }

    pub fn connection_successes(&self) -> u32 {
        self.connection_successes.load(ORD)
    }

    pub fn connection_timeouts(&self) -> u32 {
        self.connection_timeouts.load(ORD)
    }

    pub fn connection_refusals(&self) -> u32 {
        self.connection_refusals.load(ORD)
    }

    pub fn handshake_failures(&self) -> u32 {
        self.handshake_failures.load(ORD)
    }

    pub fn protocol_errors(&self) -> u32 {
        self.protocol_errors.load(ORD)
    }

    pub fn record_success(&self, latency_nanos: u64) {
        self.connection_successes.fetch_add(1, ORD);
        self.record_latency(latency_nanos);
        self.touch();
    }

    pub fn record_timeout(&self) {
        self.connection_timeouts.fetch_add(1, ORD);
        self.touch();
    }

    pub fn record_refusal(&self) {
        self.connection_refusals.fetch_add(1, ORD);
        self.touch();
    }

    pub fn record_handshake_failure(&self) {
        self.handshake_failures.fetch_add(1, ORD);
        self.touch();
    }

    pub fn record_protocol_error(&self) {
        self.protocol_errors.fetch_add(1, ORD);
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
        self.latency_sum_nanos.fetch_add(latency_nanos, ORD);
        self.latency_samples.fetch_add(1, ORD);
    }

    pub fn latency_sum_nanos(&self) -> u64 {
        self.latency_sum_nanos.load(ORD)
    }

    pub fn latency_samples(&self) -> u32 {
        self.latency_samples.load(ORD)
    }

    pub fn avg_latency_nanos(&self) -> Option<u64> {
        let samples = self.latency_samples.load(ORD);
        if samples == 0 {
            return None;
        }
        Some(self.latency_sum_nanos.load(ORD) / samples as u64)
    }

    pub fn avg_latency(&self) -> Option<std::time::Duration> {
        self.avg_latency_nanos()
            .map(std::time::Duration::from_nanos)
    }

    /// Access to protocol-specific scoring extension.
    pub fn ext(&self) -> &Ext {
        &self.ext
    }

    pub fn snapshot(&self) -> PeerScoreSnapshot<Ext::Snapshot> {
        PeerScoreSnapshot {
            score: self.score(),
            last_updated: self.last_updated(),
            connection_successes: self.connection_successes(),
            connection_timeouts: self.connection_timeouts(),
            connection_refusals: self.connection_refusals(),
            handshake_failures: self.handshake_failures(),
            protocol_errors: self.protocol_errors(),
            latency_sum_nanos: self.latency_sum_nanos(),
            latency_samples: self.latency_samples(),
            ext: self.ext.snapshot(),
        }
    }

    pub fn restore(&self, snapshot: &PeerScoreSnapshot<Ext::Snapshot>) {
        self.score.store((snapshot.score * SCORE_SCALE) as i64, ORD);
        self.last_updated.store(snapshot.last_updated, ORD);
        self.connection_successes
            .store(snapshot.connection_successes, ORD);
        self.connection_timeouts
            .store(snapshot.connection_timeouts, ORD);
        self.connection_refusals
            .store(snapshot.connection_refusals, ORD);
        self.handshake_failures
            .store(snapshot.handshake_failures, ORD);
        self.protocol_errors.store(snapshot.protocol_errors, ORD);
        self.latency_sum_nanos
            .store(snapshot.latency_sum_nanos, ORD);
        self.latency_samples.store(snapshot.latency_samples, ORD);
        self.ext.restore(&snapshot.ext);
    }
}

/// Serializable snapshot of peer score metrics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(bound(
    serialize = "ExtSnap: Serialize",
    deserialize = "ExtSnap: for<'a> Deserialize<'a>"
))]
pub struct PeerScoreSnapshot<ExtSnap = ()> {
    pub score: f64,
    pub last_updated: u64,
    pub connection_successes: u32,
    pub connection_timeouts: u32,
    pub connection_refusals: u32,
    pub handshake_failures: u32,
    pub protocol_errors: u32,
    pub latency_sum_nanos: u64,
    pub latency_samples: u32,
    /// Protocol-specific scoring extension snapshot.
    pub ext: ExtSnap,
}

impl<ExtSnap: Default> Default for PeerScoreSnapshot<ExtSnap> {
    fn default() -> Self {
        Self {
            score: 0.0,
            last_updated: 0,
            connection_successes: 0,
            connection_timeouts: 0,
            connection_refusals: 0,
            handshake_failures: 0,
            protocol_errors: 0,
            latency_sum_nanos: 0,
            latency_samples: 0,
            ext: ExtSnap::default(),
        }
    }
}

impl<ExtSnap> PeerScoreSnapshot<ExtSnap> {
    pub fn total_connection_attempts(&self) -> u32 {
        self.connection_successes
            + self.connection_timeouts
            + self.connection_refusals
            + self.handshake_failures
    }

    /// Returns 0.5 (neutral) if no attempts recorded.
    pub fn success_rate(&self) -> f64 {
        let total = self.total_connection_attempts();
        if total == 0 {
            return 0.5;
        }
        self.connection_successes as f64 / total as f64
    }

    pub fn avg_latency_nanos(&self) -> Option<u64> {
        if self.latency_samples == 0 {
            return None;
        }
        Some(self.latency_sum_nanos / self.latency_samples as u64)
    }
}

fn current_unix_timestamp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
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

        score.set_score(2_000_000.0);
        assert!((score.score() - MAX_SCORE).abs() < 0.001);

        score.set_score(-2_000_000.0);
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
        assert!((snapshot.score - 75.5).abs() < 0.01);
        assert_eq!(snapshot.connection_successes, 2);
        assert_eq!(snapshot.connection_timeouts, 1);
        assert_eq!(snapshot.connection_refusals, 1);
        assert_eq!(snapshot.protocol_errors, 1);

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

        assert!((score.score() - 1000.0).abs() < 1.0);
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
