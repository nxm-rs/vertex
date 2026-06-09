//! GCRA token-bucket rate limiting, with a single-bucket form and a per-key
//! sharded form intended for use across libp2p protocols.
//!
//! # Algorithm
//!
//! Both [`RateLimiter`] and [`KeyedRateLimiter`] implement the Generic Cell
//! Rate Algorithm (GCRA). A request asking for `n` tokens is accepted iff
//! enough time has passed since the bucket's theoretical arrival time (TAT)
//! to permit `n` token-replenishments. Each bucket needs just one timestamp
//! (`u64` nanoseconds since the limiter was constructed), so memory per key
//! is small and a check is a handful of integer ops.
//!
//! # Single vs keyed
//!
//! - [`RateLimiter`] is one bucket, [`RateLimiter::try_consume_n`] takes
//!   `&mut self`. Use it when a single owner needs throttling (for example
//!   a libp2p [`ConnectionHandler`] limiting substream-open rate on the one
//!   connection it manages).
//! - [`KeyedRateLimiter`] is `&self`-shareable via an internal mutex and
//!   maintains an independent bucket per key. Use it when many call sites
//!   (typically a `NetworkBehaviour` plus the per-connection handler readers
//!   it spawned) need to charge the same per-peer quota. Disconnect handlers
//!   should call [`KeyedRateLimiter::clear`] to release the bucket; otherwise
//!   memory grows with the count of distinct peers seen.
//!
//! [`ConnectionHandler`]: https://docs.rs/libp2p/0.56/libp2p/swarm/trait.ConnectionHandler.html

use parking_lot::Mutex;
use std::collections::HashMap;
use std::hash::Hash;
use std::num::NonZeroU32;
use std::time::{Duration, Instant};

/// Why a charge against a rate-limited bucket was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RateLimitedErr {
    /// The request asks for more tokens than the bucket's burst, so it can
    /// never be accepted - the caller must reduce the cost or fail
    /// permanently.
    #[error("rate limit cost exceeds bucket capacity")]
    TooLarge,
    /// The bucket cannot satisfy the request right now; the wrapped duration
    /// is the earliest moment a retry would succeed.
    #[error("rate limit exceeded, retry after {0:?}")]
    TooSoon(Duration),
}

/// A user-friendly quota: at most `max_tokens` may be consumed in any window
/// of `replenish_all_every`.
///
/// `Quota::n_every(NonZeroU32::new(4).unwrap(), Duration::from_secs(2))` means
/// "4 tokens every 2 seconds", which the GCRA enforces as one token every
/// 0.5 s with an instantaneous burst of 4.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quota {
    pub(crate) max_tokens: NonZeroU32,
    pub(crate) replenish_all_every: Duration,
}

impl Quota {
    /// `n` tokens every `replenish_all_every`.
    pub const fn n_every(max_tokens: NonZeroU32, replenish_all_every: Duration) -> Self {
        Self {
            max_tokens,
            replenish_all_every,
        }
    }

    /// Exactly one token per `seconds` seconds. Equivalent to a hard rate
    /// limit (no burst).
    pub const fn one_every(seconds: u64) -> Self {
        Self {
            max_tokens: NonZeroU32::MIN,
            replenish_all_every: Duration::from_secs(seconds),
        }
    }
}

/// GCRA state derived from a [`Quota`].
#[derive(Clone, Copy, Debug)]
struct Cell {
    /// Time after which the bucket is again "full" once empty - the
    /// difference between TAT and now must be at most `tau` for a request to
    /// be admitted.
    tau_nanos: u64,
    /// Nanoseconds it takes to replenish one token.
    t_nanos: u64,
}

impl Cell {
    fn from_quota(q: Quota) -> Self {
        let tau = q.replenish_all_every.as_nanos();
        // Tokens per quota interval is non-zero, so this division is
        // well-defined; both halves are saturated into a u64 because vertex
        // never configures quotas anywhere near the u64 ceiling.
        let t = tau / u128::from(q.max_tokens.get());
        Self {
            tau_nanos: u64::try_from(tau).unwrap_or(u64::MAX),
            t_nanos: u64::try_from(t).unwrap_or(u64::MAX),
        }
    }
}

/// One GCRA token-bucket guarded by `&mut self`. See the crate docs for the
/// algorithm sketch.
pub struct RateLimiter {
    cell: Cell,
    init: Instant,
    tat_nanos: u64,
}

impl RateLimiter {
    /// Build a limiter from a quota. The bucket starts full.
    pub fn new(quota: Quota) -> Self {
        Self {
            cell: Cell::from_quota(quota),
            init: Instant::now(),
            tat_nanos: 0,
        }
    }

    /// Charge one token.
    pub fn try_consume(&mut self) -> Result<(), RateLimitedErr> {
        self.try_consume_n(1)
    }

    /// Charge `n` tokens, atomically: either all `n` are charged or the
    /// bucket is left untouched.
    pub fn try_consume_n(&mut self, n: u32) -> Result<(), RateLimitedErr> {
        let now = self.init.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        match check(&self.cell, self.tat_nanos, now, n) {
            Ok(new_tat) => {
                self.tat_nanos = new_tat;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}

/// A [`RateLimiter`] per key, shareable via `&self` through an internal mutex.
/// Buckets are lazily inserted on first use; call [`Self::clear`] to release
/// them on peer disconnect.
pub struct KeyedRateLimiter<K: Eq + Hash> {
    cell: Cell,
    init: Instant,
    tat_per_key: Mutex<HashMap<K, u64>>,
}

impl<K: Eq + Hash> KeyedRateLimiter<K> {
    pub fn new(quota: Quota) -> Self {
        Self {
            cell: Cell::from_quota(quota),
            init: Instant::now(),
            tat_per_key: Mutex::new(HashMap::new()),
        }
    }

    /// Charge one token against `key`.
    pub fn try_consume(&self, key: K) -> Result<(), RateLimitedErr> {
        self.try_consume_n(key, 1)
    }

    /// Charge `n` tokens against `key`, atomically: either all `n` are
    /// charged or the bucket is left untouched.
    pub fn try_consume_n(&self, key: K, n: u32) -> Result<(), RateLimitedErr> {
        let now = self.init.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        let mut guard = self.tat_per_key.lock();
        let tat = guard.entry(key).or_insert(0);
        match check(&self.cell, *tat, now, n) {
            Ok(new_tat) => {
                *tat = new_tat;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Drop the bucket for `key` to release memory; call this on the final
    /// disconnect for that peer.
    pub fn clear(&self, key: &K) {
        self.tat_per_key.lock().remove(key);
    }

    /// Drop every bucket whose TAT lies in the past, i.e. that is already
    /// fully replenished. Lighthouse-style periodic cleanup; useful when
    /// disconnect events are not observed (or as a backstop).
    pub fn retain_recent(&self) {
        let now = self.init.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        self.tat_per_key.lock().retain(|_, tat| *tat > now);
    }

    pub fn tracked_keys(&self) -> usize {
        self.tat_per_key.lock().len()
    }
}

/// Shared GCRA admission check. Returns the new TAT on success.
fn check(cell: &Cell, tat: u64, now: u64, n: u32) -> Result<u64, RateLimitedErr> {
    let cost = cell.t_nanos.saturating_mul(u64::from(n));
    if cost > cell.tau_nanos {
        // Cost exceeds the bucket capacity; no amount of waiting will help.
        return Err(RateLimitedErr::TooLarge);
    }
    let earliest_admit = tat.saturating_add(cost).saturating_sub(cell.tau_nanos);
    if now < earliest_admit {
        return Err(RateLimitedErr::TooSoon(Duration::from_nanos(
            earliest_admit.saturating_sub(now),
        )));
    }
    Ok(now.max(tat).saturating_add(cost))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn quota_n_per(n: u32, seconds: u64) -> Quota {
        Quota::n_every(NonZeroU32::new(n).unwrap(), Duration::from_secs(seconds))
    }

    #[test]
    fn single_bucket_burst_then_exhaustion() {
        let mut rl = RateLimiter::new(quota_n_per(3, 1));
        assert!(rl.try_consume().is_ok());
        assert!(rl.try_consume().is_ok());
        assert!(rl.try_consume().is_ok());
        assert!(matches!(rl.try_consume(), Err(RateLimitedErr::TooSoon(_))));
    }

    #[test]
    fn try_consume_n_is_atomic() {
        let mut rl = RateLimiter::new(quota_n_per(5, 60));
        assert!(rl.try_consume_n(5).is_ok());
        // Bucket exhausted; a single token must not partially drain.
        assert!(matches!(rl.try_consume(), Err(RateLimitedErr::TooSoon(_))));
    }

    #[test]
    fn cost_greater_than_burst_is_too_large() {
        let mut rl = RateLimiter::new(quota_n_per(4, 1));
        assert_eq!(rl.try_consume_n(5), Err(RateLimitedErr::TooLarge));
    }

    #[test]
    fn keyed_per_peer_independence() {
        let rl = KeyedRateLimiter::<&'static str>::new(quota_n_per(2, 60));
        assert!(rl.try_consume("alice").is_ok());
        assert!(rl.try_consume("alice").is_ok());
        assert!(matches!(
            rl.try_consume("alice"),
            Err(RateLimitedErr::TooSoon(_))
        ));
        // Bob is unaffected.
        assert!(rl.try_consume("bob").is_ok());
    }

    #[test]
    fn keyed_clear_releases_bucket() {
        let rl = KeyedRateLimiter::<&'static str>::new(quota_n_per(1, 60));
        assert!(rl.try_consume("x").is_ok());
        assert!(matches!(
            rl.try_consume("x"),
            Err(RateLimitedErr::TooSoon(_))
        ));
        assert_eq!(rl.tracked_keys(), 1);
        rl.clear(&"x");
        assert_eq!(rl.tracked_keys(), 0);
        // Fresh bucket after clear.
        assert!(rl.try_consume("x").is_ok());
    }

    #[test]
    fn retain_recent_drops_replenished_buckets() {
        let rl = KeyedRateLimiter::<&'static str>::new(quota_n_per(1, 0));
        // A 0-second quota means a token is replenished essentially
        // instantly; every charge succeeds. The corresponding bucket is also
        // "fully replenished" immediately and retain_recent will drop it.
        assert!(rl.try_consume("x").is_ok());
        rl.retain_recent();
        assert_eq!(rl.tracked_keys(), 0);
    }

    #[test]
    fn keyed_too_large() {
        let rl = KeyedRateLimiter::<u32>::new(quota_n_per(2, 1));
        assert_eq!(rl.try_consume_n(0, 3), Err(RateLimitedErr::TooLarge));
    }

    #[test]
    fn too_soon_reports_wait_duration() {
        let mut rl = RateLimiter::new(quota_n_per(1, 60));
        assert!(rl.try_consume().is_ok());
        match rl.try_consume() {
            Err(RateLimitedErr::TooSoon(d)) => {
                assert!(d > Duration::ZERO);
                assert!(d <= Duration::from_secs(60));
            }
            other => panic!("expected TooSoon, got {other:?}"),
        }
    }
}
