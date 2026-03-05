//! Lock-free peer scoring with atomics.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering, fence};
use std::time::Duration;

use crate::snapshot::PeerScoreSnapshot;

/// Fixed-point multiplier for storing f64 scores as i64 atomics.
const SCORE_SCALE: f64 = 100_000.0;
/// Minimum allowed score (matches ban threshold scale).
const MIN_SCORE: f64 = -100.0;
/// Maximum allowed score (symmetric with minimum).
const MAX_SCORE: f64 = 100.0;

/// Lock-free peer scoring using atomics for concurrent access.
pub struct PeerScore {
    score: AtomicI64,
    latency_sum_nanos: AtomicU64,
    latency_samples: AtomicU32,
}

impl Default for PeerScore {
    fn default() -> Self {
        Self::new()
    }
}

impl PeerScore {
    pub fn new() -> Self {
        Self {
            score: AtomicI64::new(0),
            latency_sum_nanos: AtomicU64::new(0),
            latency_samples: AtomicU32::new(0),
        }
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
                break;
            }
        }
    }

    pub fn set_score(&self, score: f64) {
        let clamped = score.clamp(MIN_SCORE, MAX_SCORE);
        self.score.store((clamped * SCORE_SCALE) as i64, Ordering::Release);
    }

    pub fn should_ban(&self, threshold: f64) -> bool {
        self.score() < threshold
    }

    pub fn record_latency(&self, latency_nanos: u64) {
        self.latency_sum_nanos.fetch_add(latency_nanos, Ordering::Relaxed);
        self.latency_samples.fetch_add(1, Ordering::Release);
    }

    /// Average latency in nanoseconds, or None if no samples recorded.
    pub fn avg_latency_nanos(&self) -> Option<u64> {
        let samples = self.latency_samples.load(Ordering::Acquire);
        if samples == 0 {
            return None;
        }
        Some(self.latency_sum_nanos.load(Ordering::Relaxed) / samples as u64)
    }

    pub fn avg_latency(&self) -> Option<Duration> {
        self.avg_latency_nanos().map(Duration::from_nanos)
    }
}

impl From<&PeerScore> for PeerScoreSnapshot {
    fn from(score: &PeerScore) -> Self {
        fence(Ordering::Acquire);
        Self {
            score: score.score.load(Ordering::Relaxed) as f64 / SCORE_SCALE,
            latency_sum_nanos: score.latency_sum_nanos.load(Ordering::Relaxed),
            latency_samples: score.latency_samples.load(Ordering::Relaxed),
        }
    }
}

impl From<&Arc<PeerScore>> for PeerScoreSnapshot {
    fn from(score: &Arc<PeerScore>) -> Self {
        Self::from(score.as_ref())
    }
}

impl From<&PeerScoreSnapshot> for PeerScore {
    fn from(snapshot: &PeerScoreSnapshot) -> Self {
        Self {
            score: AtomicI64::new((snapshot.score * SCORE_SCALE) as i64),
            latency_sum_nanos: AtomicU64::new(snapshot.latency_sum_nanos),
            latency_samples: AtomicU32::new(snapshot.latency_samples),
        }
    }
}

impl From<PeerScoreSnapshot> for PeerScore {
    fn from(snapshot: PeerScoreSnapshot) -> Self {
        Self::from(&snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_new_score() {
        let score = PeerScore::new();
        assert_eq!(score.score(), 0.0);
    }

    #[test]
    fn test_add_score() {
        let score = PeerScore::new();

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
    fn test_latency_averaging() {
        let score = PeerScore::new();

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
        let score = PeerScore::new();

        score.set_score(75.5);
        score.record_latency(100_000_000);
        score.record_latency(200_000_000);

        let snapshot = PeerScoreSnapshot::from(&score);
        assert!((snapshot.score - 75.5).abs() < 0.01);

        let score2 = PeerScore::from(&snapshot);
        assert!((score2.score() - 75.5).abs() < 0.01);
        assert_eq!(score2.avg_latency_nanos(), Some(150_000_000));
    }

    #[test]
    fn test_concurrent_updates() {
        let score = Arc::new(PeerScore::new());
        let mut handles = vec![];

        for _ in 0..10 {
            let score = Arc::clone(&score);
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    score.add_score(1.0);
                    score.record_latency(1_000_000);
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert!((score.score() - MAX_SCORE).abs() < 0.01);
    }

    #[test]
    fn test_should_ban() {
        let score = PeerScore::new();

        score.set_score(-50.0);
        assert!(score.should_ban(-40.0));
        assert!(!score.should_ban(-60.0));
    }
}
