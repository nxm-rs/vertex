//! Token bucket rate limiter for per-connection stream acceptance.

use std::time::{Duration, Instant};

/// Token bucket rate limiter for inbound stream acceptance.
///
/// Prevents peers from rapidly cycling streams even when each stream completes
/// quickly. Tokens replenish at a steady rate up to a configurable burst limit.
/// Check this *before* any concurrent-stream bound (e.g. `FuturesSet::try_push`)
/// so that rapid open/close cycles are rejected even when the concurrent set has
/// capacity.
pub struct RateLimiter {
    tokens: f64,
    max_tokens: f64,
    refill_rate: f64,
    last_refill: Instant,
}

impl RateLimiter {
    /// Create a rate limiter with the given burst capacity and per-token refill interval.
    ///
    /// `max_tokens` is the burst allowance. `refill_interval` is the time for
    /// one token to regenerate. Starts full (at `max_tokens`).
    pub fn new(max_tokens: u32, refill_interval: Duration) -> Self {
        let max = max_tokens as f64;
        Self {
            tokens: max,
            max_tokens: max,
            refill_rate: 1.0 / refill_interval.as_secs_f64(),
            last_refill: Instant::now(),
        }
    }

    /// Try to consume one token. Returns `true` if accepted, `false` if rate limited.
    pub fn try_acquire(&mut self) -> bool {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.max_tokens);
        self.last_refill = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_then_exhaustion() {
        let mut rl = RateLimiter::new(3, Duration::from_secs(1));
        assert!(rl.try_acquire());
        assert!(rl.try_acquire());
        assert!(rl.try_acquire());
        assert!(!rl.try_acquire());
    }

    #[test]
    fn refill_after_elapsed_time() {
        let mut rl = RateLimiter::new(2, Duration::from_millis(100));
        // Drain burst
        assert!(rl.try_acquire());
        assert!(rl.try_acquire());
        assert!(!rl.try_acquire());

        // Simulate time passing by backdating last_refill
        rl.last_refill -= Duration::from_millis(150);
        assert!(rl.try_acquire()); // 1 token refilled
        assert!(!rl.try_acquire()); // not enough time for another
    }

    #[test]
    fn does_not_exceed_max_tokens() {
        let mut rl = RateLimiter::new(2, Duration::from_millis(10));
        // Wait a long time (simulated)
        rl.last_refill -= Duration::from_secs(60);
        assert!(rl.try_acquire());
        assert!(rl.try_acquire());
        assert!(!rl.try_acquire()); // Capped at max_tokens=2
    }
}
