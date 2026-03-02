//! Exponential backoff calculation for peer dial attempts.
//!
//! Backoff uses per-peer jitter derived from a stable seed (e.g. overlay address bytes)
//! to prevent synchronized retry storms when many peers fail at once.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Base backoff duration in seconds (30 seconds).
pub const DEFAULT_BASE_BACKOFF_SECS: u64 = 30;
/// Maximum backoff duration in seconds (1 hour).
///
/// Exponential growth: 30s → 60s → 120s → 240s → 480s → 960s → 1920s → 3600s (cap at failure #8).
/// With ±25% jitter, peers at max backoff retry every 45-75 minutes.
pub const DEFAULT_MAX_BACKOFF_SECS: u64 = 3600;

/// Per-peer backoff state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackoffState {
    /// Unix timestamp of last dial attempt.
    pub last_attempt: u64,
    /// Consecutive dial failures.
    pub consecutive_failures: u32,
}

impl BackoffState {
    pub fn new(last_attempt: u64, consecutive_failures: u32) -> Self {
        Self {
            last_attempt,
            consecutive_failures,
        }
    }

    /// Calculate remaining backoff duration without jitter.
    pub fn remaining(&self, now: u64, base_secs: u64, max_secs: u64) -> Option<Duration> {
        self.remaining_inner(now, base_secs, max_secs, None)
    }

    /// Calculate remaining backoff with per-peer jitter (±25%).
    ///
    /// The `jitter_seed` should be stable per-peer (e.g. derived from overlay address)
    /// so the same peer always gets the same jitter factor, but different peers spread
    /// their retry times apart.
    pub fn remaining_jittered(
        &self,
        now: u64,
        base_secs: u64,
        max_secs: u64,
        jitter_seed: u64,
    ) -> Option<Duration> {
        self.remaining_inner(now, base_secs, max_secs, Some(jitter_seed))
    }

    fn remaining_inner(
        &self,
        now: u64,
        base_secs: u64,
        max_secs: u64,
        jitter_seed: Option<u64>,
    ) -> Option<Duration> {
        if self.consecutive_failures == 0 {
            return None;
        }

        // Exponential backoff: base * 2^(failures-1), capped at max
        let base_backoff = base_secs
            .saturating_mul(1u64 << (self.consecutive_failures - 1).min(10))
            .min(max_secs);

        let backoff_secs = match jitter_seed {
            Some(seed) => {
                // Deterministic jitter: ±25% based on seed mixed with failure count.
                // Knuth multiplicative hash to spread bits.
                let mixed = seed
                    .wrapping_mul(0x517c_c1b7_2722_0a95)
                    .wrapping_add(self.consecutive_failures as u64);
                // Map upper 16 bits to [0.75, 1.25)
                let frac = (mixed >> 48) as f64 / 65536.0;
                let factor = 0.75 + frac * 0.5;
                (base_backoff as f64 * factor) as u64
            }
            None => base_backoff,
        };

        let backoff_until = self.last_attempt.saturating_add(backoff_secs);

        if now >= backoff_until {
            None
        } else {
            Some(Duration::from_secs(backoff_until - now))
        }
    }

    /// Check if currently in backoff.
    pub fn is_in_backoff(&self, now: u64, base_secs: u64, max_secs: u64) -> bool {
        self.remaining(now, base_secs, max_secs).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_backoff_zero_failures() {
        let state = BackoffState::new(1000, 0);
        assert!(state
            .remaining(1000, DEFAULT_BASE_BACKOFF_SECS, DEFAULT_MAX_BACKOFF_SECS)
            .is_none());
        assert!(!state.is_in_backoff(1000, DEFAULT_BASE_BACKOFF_SECS, DEFAULT_MAX_BACKOFF_SECS));
    }

    #[test]
    fn test_exponential_growth() {
        let base = DEFAULT_BASE_BACKOFF_SECS;
        let max = DEFAULT_MAX_BACKOFF_SECS;

        // 1 failure: 30s backoff
        let s1 = BackoffState::new(1000, 1);
        let r1 = s1.remaining(1000, base, max).unwrap();
        assert_eq!(r1.as_secs(), 30);

        // 2 failures: 60s backoff
        let s2 = BackoffState::new(1000, 2);
        let r2 = s2.remaining(1000, base, max).unwrap();
        assert_eq!(r2.as_secs(), 60);

        // 3 failures: 120s backoff
        let s3 = BackoffState::new(1000, 3);
        let r3 = s3.remaining(1000, base, max).unwrap();
        assert_eq!(r3.as_secs(), 120);
    }

    #[test]
    fn test_max_cap() {
        let base = DEFAULT_BASE_BACKOFF_SECS;
        let max = DEFAULT_MAX_BACKOFF_SECS;

        // Many failures should cap at max (3600s)
        let state = BackoffState::new(1000, 20);
        let remaining = state.remaining(1000, base, max).unwrap();
        assert_eq!(remaining.as_secs(), max);
    }

    #[test]
    fn test_custom_base_and_max() {
        let state = BackoffState::new(1000, 1);
        let remaining = state.remaining(1000, 10, 500).unwrap();
        assert_eq!(remaining.as_secs(), 10);

        let state2 = BackoffState::new(1000, 5);
        let remaining2 = state2.remaining(1000, 10, 500).unwrap();
        // 10 * 2^4 = 160
        assert_eq!(remaining2.as_secs(), 160);
    }

    #[test]
    fn test_serde_roundtrip() {
        let state = BackoffState::new(1000, 3);
        let json = serde_json::to_string(&state).unwrap();
        let restored: BackoffState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, restored);
    }

    #[test]
    fn test_expired_backoff() {
        let state = BackoffState::new(1000, 1);
        // 30s after last attempt + 30s backoff = expired
        assert!(state
            .remaining(1031, DEFAULT_BASE_BACKOFF_SECS, DEFAULT_MAX_BACKOFF_SECS)
            .is_none());
    }

    #[test]
    fn test_jitter_within_bounds() {
        let base = DEFAULT_BASE_BACKOFF_SECS;
        let max = DEFAULT_MAX_BACKOFF_SECS;

        // Test many seeds; all should produce jitter in ±25% of base backoff.
        for seed in 0u64..1000 {
            let state = BackoffState::new(1000, 1);
            let remaining = state.remaining_jittered(1000, base, max, seed).unwrap();
            let secs = remaining.as_secs();
            // base=30, ±25% → [22, 37]
            assert!(
                secs >= 22 && secs <= 37,
                "seed {seed}: backoff {secs}s outside [22, 37]"
            );
        }
    }

    #[test]
    fn test_jitter_deterministic_per_seed() {
        let state = BackoffState::new(1000, 2);
        let base = DEFAULT_BASE_BACKOFF_SECS;
        let max = DEFAULT_MAX_BACKOFF_SECS;

        let r1 = state.remaining_jittered(1000, base, max, 42).unwrap();
        let r2 = state.remaining_jittered(1000, base, max, 42).unwrap();
        assert_eq!(r1, r2, "same seed should produce same jitter");
    }

    #[test]
    fn test_jitter_varies_across_seeds() {
        let state = BackoffState::new(1000, 3);
        let base = DEFAULT_BASE_BACKOFF_SECS;
        let max = DEFAULT_MAX_BACKOFF_SECS;

        let r1 = state.remaining_jittered(1000, base, max, 1).unwrap();
        let r2 = state.remaining_jittered(1000, base, max, 999).unwrap();
        // Different seeds should (very likely) produce different jitter
        assert_ne!(r1, r2, "different seeds should produce different jitter");
    }

    #[test]
    fn test_jitter_capped_at_max() {
        let base = DEFAULT_BASE_BACKOFF_SECS;
        let max = DEFAULT_MAX_BACKOFF_SECS;

        // Many failures: jittered backoff should still cap near max (±25%)
        for seed in 0u64..100 {
            let state = BackoffState::new(1000, 20);
            let remaining = state.remaining_jittered(1000, base, max, seed).unwrap();
            let secs = remaining.as_secs();
            // max=3600, ±25% → [2700, 4500]
            assert!(
                secs >= 2700 && secs <= 4500,
                "seed {seed}: capped backoff {secs}s outside [2700, 4500]"
            );
        }
    }
}
