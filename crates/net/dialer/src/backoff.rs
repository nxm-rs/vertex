//! Inline exponential backoff with deterministic jitter.

use std::hash::{Hash, Hasher};
use std::time::Duration;

/// Tracked state for a single peer in the backoff cache.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct BackoffEntry {
    /// Monotonic timestamp (seconds since tracker creation) of the last failure.
    pub last_failure_secs: u64,
    /// Number of consecutive failures recorded.
    pub consecutive_failures: u32,
}

/// Compute remaining backoff duration, or `None` if the backoff has expired.
///
/// Backoff = min(base_secs * 2^(failures-1), max_secs) + jitter.
/// Jitter is ±12.5% derived from `jitter_seed` to keep it deterministic per-peer.
pub(crate) fn backoff_remaining(
    entry: &BackoffEntry,
    now_secs: u64,
    base_secs: u64,
    max_secs: u64,
    jitter_seed: u64,
) -> Option<Duration> {
    if entry.consecutive_failures == 0 {
        return None;
    }
    let exp = (entry.consecutive_failures - 1).min(10);
    let raw = base_secs.saturating_mul(1u64 << exp).min(max_secs);

    // ±12.5% jitter derived from seed
    let jitter_frac = (jitter_seed % 256) as i64 - 128; // [-128, 127]
    let jitter = (raw as i64 * jitter_frac) / 1024;
    let backoff = (raw as i64 + jitter).max(1) as u64;

    let elapsed = now_secs.saturating_sub(entry.last_failure_secs);
    if elapsed >= backoff {
        None
    } else {
        Some(Duration::from_secs(backoff - elapsed))
    }
}

/// Produce a deterministic jitter seed from any hashable Id.
pub(crate) fn jitter_seed_for<Id: Hash>(id: &Id) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_failures_no_backoff() {
        let entry = BackoffEntry::default();
        assert!(backoff_remaining(&entry, 100, 5, 20, 0).is_none());
    }

    #[test]
    fn first_failure_uses_base() {
        let entry = BackoffEntry {
            last_failure_secs: 100,
            consecutive_failures: 1,
        };
        // At t=100, backoff ~5s (±jitter). Check still active at t=101.
        assert!(backoff_remaining(&entry, 101, 5, 20, 128).is_some());
        // At t=106, should be expired.
        assert!(backoff_remaining(&entry, 106, 5, 20, 128).is_none());
    }

    #[test]
    fn second_failure_doubles() {
        let entry = BackoffEntry {
            last_failure_secs: 100,
            consecutive_failures: 2,
        };
        // base=5, 2^1=2, raw=10. At t=105, should still be active.
        assert!(backoff_remaining(&entry, 105, 5, 20, 128).is_some());
        // At t=112, should be expired.
        assert!(backoff_remaining(&entry, 112, 5, 20, 128).is_none());
    }

    #[test]
    fn capped_at_max() {
        let entry = BackoffEntry {
            last_failure_secs: 100,
            consecutive_failures: 10,
        };
        // Without cap this would be huge, but max=20.
        assert!(backoff_remaining(&entry, 122, 5, 20, 128).is_none());
    }

    #[test]
    fn jitter_seed_deterministic() {
        let a = jitter_seed_for(&42u64);
        let b = jitter_seed_for(&42u64);
        assert_eq!(a, b);
        let c = jitter_seed_for(&43u64);
        assert_ne!(a, c);
    }
}
