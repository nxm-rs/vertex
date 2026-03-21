//! Lock-free exponential backoff for peer dial attempts.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};
use std::time::Duration;

/// Lock-free exponential backoff using atomics.
///
/// Tracks consecutive failures and last attempt timestamp. Rebuilt from
/// persisted fields each session — not itself serialized.
pub struct PeerBackoff {
    last_attempt: AtomicU64,
    consecutive_failures: AtomicU32,
}

impl Clone for PeerBackoff {
    fn clone(&self) -> Self {
        // Best-effort consistency: prevents reordering of the loads below
        // with prior operations. Does not guarantee a consistent snapshot
        // across both fields (acceptable for approximate backoff checks).
        fence(Ordering::Acquire);
        Self {
            last_attempt: AtomicU64::new(self.last_attempt.load(Ordering::Relaxed)),
            consecutive_failures: AtomicU32::new(self.consecutive_failures.load(Ordering::Relaxed)),
        }
    }
}

impl Default for PeerBackoff {
    fn default() -> Self {
        Self::new()
    }
}

impl PeerBackoff {
    /// Base backoff duration (30 seconds).
    pub const DEFAULT_BASE_SECS: u64 = 30;

    /// Maximum backoff duration (1 hour).
    pub const DEFAULT_MAX_SECS: u64 = 3600;

    pub fn new() -> Self {
        Self {
            last_attempt: AtomicU64::new(0),
            consecutive_failures: AtomicU32::new(0),
        }
    }

    /// Restore from persisted fields.
    pub fn from_persisted(last_attempt: u64, consecutive_failures: u32) -> Self {
        Self {
            last_attempt: AtomicU64::new(last_attempt),
            consecutive_failures: AtomicU32::new(consecutive_failures),
        }
    }

    /// Record a dial failure: increments consecutive failures and stores the attempt timestamp.
    pub fn record_failure(&self, now_secs: u64) {
        self.consecutive_failures.fetch_add(1, Ordering::Relaxed);
        self.last_attempt.store(now_secs, Ordering::Relaxed);
    }

    /// Reset after a successful connection.
    pub fn reset(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
    }

    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures.load(Ordering::Relaxed)
    }

    pub fn last_attempt(&self) -> u64 {
        self.last_attempt.load(Ordering::Relaxed)
    }

    /// Calculate remaining backoff with per-peer jitter (+/-25%) and custom parameters.
    ///
    /// The `jitter_seed` should be stable per-peer (e.g. derived from overlay address)
    /// so the same peer always gets the same jitter factor, but different peers spread
    /// their retry times apart.
    pub fn remaining_jittered_with(
        &self,
        now: u64,
        base_secs: u64,
        max_secs: u64,
        jitter_seed: u64,
    ) -> Option<Duration> {
        remaining_inner(
            self.consecutive_failures(),
            self.last_attempt(),
            now,
            base_secs,
            max_secs,
            Some(jitter_seed),
        )
    }

    /// Calculate remaining backoff without jitter, with custom parameters.
    pub fn remaining_with(&self, now: u64, base_secs: u64, max_secs: u64) -> Option<Duration> {
        remaining_inner(
            self.consecutive_failures(),
            self.last_attempt(),
            now,
            base_secs,
            max_secs,
            None,
        )
    }

    /// Calculate remaining backoff with per-peer jitter (+/-25%).
    pub fn remaining_jittered(&self, now: u64, jitter_seed: u64) -> Option<Duration> {
        self.remaining_jittered_with(
            now,
            Self::DEFAULT_BASE_SECS,
            Self::DEFAULT_MAX_SECS,
            jitter_seed,
        )
    }

    /// Calculate remaining backoff without jitter.
    pub fn remaining(&self, now: u64) -> Option<Duration> {
        self.remaining_with(now, Self::DEFAULT_BASE_SECS, Self::DEFAULT_MAX_SECS)
    }
}

fn remaining_inner(
    consecutive_failures: u32,
    last_attempt: u64,
    now: u64,
    base_secs: u64,
    max_secs: u64,
    jitter_seed: Option<u64>,
) -> Option<Duration> {
    if consecutive_failures == 0 {
        return None;
    }

    // Exponential backoff: base * 2^(failures-1), capped at max
    let base_backoff = base_secs
        .saturating_mul(1u64 << (consecutive_failures - 1).min(10))
        .min(max_secs);

    let backoff_secs = match jitter_seed {
        Some(seed) => {
            // Deterministic jitter: +/-25% based on seed mixed with failure count.
            // Knuth multiplicative hash to spread bits.
            let mixed = seed
                .wrapping_mul(0x517c_c1b7_2722_0a95)
                .wrapping_add(consecutive_failures as u64);
            // Map upper 16 bits to [0.75, 1.25)
            let frac = (mixed >> 48) as f64 / 65536.0;
            let factor = 0.75 + frac * 0.5;
            (base_backoff as f64 * factor) as u64
        }
        None => base_backoff,
    };

    let backoff_until = last_attempt.saturating_add(backoff_secs);

    if now >= backoff_until {
        None
    } else {
        Some(Duration::from_secs(backoff_until - now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u64 = PeerBackoff::DEFAULT_BASE_SECS;
    const MAX: u64 = PeerBackoff::DEFAULT_MAX_SECS;

    #[test]
    fn no_backoff_zero_failures() {
        let b = PeerBackoff::new();
        assert!(b.remaining(1000).is_none());
    }

    #[test]
    fn exponential_growth() {
        // 1 failure: 30s
        let b1 = PeerBackoff::from_persisted(1000, 1);
        assert_eq!(b1.remaining_with(1000, BASE, MAX).unwrap().as_secs(), 30);

        // 2 failures: 60s
        let b2 = PeerBackoff::from_persisted(1000, 2);
        assert_eq!(b2.remaining_with(1000, BASE, MAX).unwrap().as_secs(), 60);

        // 3 failures: 120s
        let b3 = PeerBackoff::from_persisted(1000, 3);
        assert_eq!(b3.remaining_with(1000, BASE, MAX).unwrap().as_secs(), 120);
    }

    #[test]
    fn max_cap() {
        let b = PeerBackoff::from_persisted(1000, 20);
        assert_eq!(b.remaining_with(1000, BASE, MAX).unwrap().as_secs(), MAX);
    }

    #[test]
    fn custom_base_and_max() {
        let b1 = PeerBackoff::from_persisted(1000, 1);
        assert_eq!(b1.remaining_with(1000, 10, 500).unwrap().as_secs(), 10);

        let b2 = PeerBackoff::from_persisted(1000, 5);
        // 10 * 2^4 = 160
        assert_eq!(b2.remaining_with(1000, 10, 500).unwrap().as_secs(), 160);
    }

    #[test]
    fn expired_backoff() {
        let b = PeerBackoff::from_persisted(1000, 1);
        assert!(b.remaining_with(1031, BASE, MAX).is_none());
    }

    #[test]
    fn jitter_within_bounds() {
        for seed in 0u64..1000 {
            let b = PeerBackoff::from_persisted(1000, 1);
            let remaining = b.remaining_jittered(1000, seed).unwrap();
            let secs = remaining.as_secs();
            // base=30, +/-25% -> [22, 37]
            assert!(
                (22..=37).contains(&secs),
                "seed {seed}: backoff {secs}s outside [22, 37]"
            );
        }
    }

    #[test]
    fn jitter_deterministic_per_seed() {
        let b = PeerBackoff::from_persisted(1000, 2);
        let r1 = b.remaining_jittered(1000, 42).unwrap();
        let r2 = b.remaining_jittered(1000, 42).unwrap();
        assert_eq!(r1, r2, "same seed should produce same jitter");
    }

    #[test]
    fn jitter_varies_across_seeds() {
        let b = PeerBackoff::from_persisted(1000, 3);
        let r1 = b.remaining_jittered(1000, 1).unwrap();
        let r2 = b.remaining_jittered(1000, 999).unwrap();
        assert_ne!(r1, r2, "different seeds should produce different jitter");
    }

    #[test]
    fn jitter_capped_at_max() {
        for seed in 0u64..100 {
            let b = PeerBackoff::from_persisted(1000, 20);
            let remaining = b.remaining_jittered(1000, seed).unwrap();
            let secs = remaining.as_secs();
            // max=3600, +/-25% -> [2700, 4500]
            assert!(
                (2700..=4500).contains(&secs),
                "seed {seed}: capped backoff {secs}s outside [2700, 4500]"
            );
        }
    }

    #[test]
    fn record_failure_increments() {
        let b = PeerBackoff::new();
        assert_eq!(b.consecutive_failures(), 0);

        b.record_failure(100);
        assert_eq!(b.consecutive_failures(), 1);
        assert_eq!(b.last_attempt(), 100);

        b.record_failure(200);
        assert_eq!(b.consecutive_failures(), 2);
        assert_eq!(b.last_attempt(), 200);
    }

    #[test]
    fn reset_clears_failures() {
        let b = PeerBackoff::from_persisted(1000, 5);
        assert_eq!(b.consecutive_failures(), 5);

        b.reset();
        assert_eq!(b.consecutive_failures(), 0);
    }

    #[test]
    fn clone_preserves_state() {
        let b = PeerBackoff::from_persisted(500, 3);
        let b2 = b.clone();
        assert_eq!(b2.consecutive_failures(), 3);
        assert_eq!(b2.last_attempt(), 500);
    }
}
