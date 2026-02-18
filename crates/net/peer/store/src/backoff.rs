//! Exponential backoff calculation for peer dial attempts.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Base backoff duration in seconds (30 seconds).
pub const DEFAULT_BASE_BACKOFF_SECS: u64 = 30;
/// Maximum backoff duration in seconds (1 hour).
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

    /// Calculate remaining backoff duration with custom base and max.
    pub fn remaining(&self, now: u64, base_secs: u64, max_secs: u64) -> Option<Duration> {
        if self.consecutive_failures == 0 {
            return None;
        }

        // Exponential backoff: base * 2^(failures-1), capped at max
        let backoff_secs = base_secs
            .saturating_mul(1u64 << (self.consecutive_failures - 1).min(10))
            .min(max_secs);

        let backoff_until = self.last_attempt.saturating_add(backoff_secs);

        if now >= backoff_until {
            None
        } else {
            Some(Duration::from_secs(backoff_until - now))
        }
    }

    /// Check if currently in backoff with custom base and max.
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
        assert!(state.remaining(1000, DEFAULT_BASE_BACKOFF_SECS, DEFAULT_MAX_BACKOFF_SECS).is_none());
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

        // Many failures should cap at max
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
        assert!(state.remaining(1031, DEFAULT_BASE_BACKOFF_SECS, DEFAULT_MAX_BACKOFF_SECS).is_none());
    }
}
