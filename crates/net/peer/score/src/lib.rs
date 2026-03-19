//! Lock-free peer scoring with atomics.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};
use std::time::Duration;

use portable_atomic::AtomicF64;
use serde::{Deserialize, Serialize};

const MIN_SCORE: f64 = -100.0;
const MAX_SCORE: f64 = 100.0;

/// Lock-free peer scoring using atomics for concurrent access.
#[derive(Debug, Serialize, Deserialize)]
pub struct PeerScore {
    score: AtomicF64,
    latency_sum_nanos: AtomicU64,
    latency_samples: AtomicU32,
}

impl Clone for PeerScore {
    fn clone(&self) -> Self {
        fence(Ordering::Acquire);
        Self {
            score: AtomicF64::new(self.score.load(Ordering::Relaxed)),
            latency_sum_nanos: AtomicU64::new(self.latency_sum_nanos.load(Ordering::Relaxed)),
            latency_samples: AtomicU32::new(self.latency_samples.load(Ordering::Relaxed)),
        }
    }
}

impl Default for PeerScore {
    fn default() -> Self {
        Self::new()
    }
}

impl PeerScore {
    pub fn new() -> Self {
        Self {
            score: AtomicF64::new(0.0),
            latency_sum_nanos: AtomicU64::new(0),
            latency_samples: AtomicU32::new(0),
        }
    }

    pub fn score(&self) -> f64 {
        self.score.load(Ordering::Acquire)
    }

    /// Atomically adjust score, clamped to bounds.
    pub fn add_score(&self, delta: f64) {
        loop {
            let current = self.score.load(Ordering::Acquire);
            let new_val = (current + delta).clamp(MIN_SCORE, MAX_SCORE);
            if self
                .score
                .compare_exchange_weak(current, new_val, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
    }

    pub fn set_score(&self, score: f64) {
        let clamped = score.clamp(MIN_SCORE, MAX_SCORE);
        self.score.store(clamped, Ordering::Release);
    }

    pub fn should_ban(&self, threshold: f64) -> bool {
        self.score() < threshold
    }

    pub fn record_latency(&self, latency_nanos: u64) {
        self.latency_sum_nanos
            .fetch_add(latency_nanos, Ordering::Relaxed);
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
    fn test_clone_roundtrip() {
        let score = PeerScore::new();

        score.set_score(75.5);
        score.record_latency(100_000_000);
        score.record_latency(200_000_000);

        let score2 = score.clone();
        assert!((score2.score() - 75.5).abs() < 0.01);
        assert_eq!(score2.avg_latency_nanos(), Some(150_000_000));
    }

    #[test]
    fn test_serde_roundtrip() {
        let score = PeerScore::new();
        score.set_score(75.5);
        score.record_latency(100_000_000);
        score.record_latency(200_000_000);

        let bytes = postcard::to_allocvec(&score).unwrap();
        let restored: PeerScore = postcard::from_bytes(&bytes).unwrap();
        assert!((restored.score() - 75.5).abs() < 0.01);
        assert_eq!(restored.avg_latency_nanos(), Some(150_000_000));
    }

    #[test]
    fn test_backward_compat() {
        // Legacy format: f64, u64, u32 — identical wire layout to AtomicF64, AtomicU64, AtomicU32.
        #[derive(serde::Serialize, serde::Deserialize)]
        struct Legacy {
            score: f64,
            latency_sum_nanos: u64,
            latency_samples: u32,
        }

        let legacy = Legacy {
            score: 42.5,
            latency_sum_nanos: 300_000_000,
            latency_samples: 3,
        };
        let bytes = postcard::to_allocvec(&legacy).unwrap();
        let restored: PeerScore = postcard::from_bytes(&bytes).unwrap();
        assert!((restored.score() - 42.5).abs() < 0.01);
        assert_eq!(restored.avg_latency_nanos(), Some(100_000_000));
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
