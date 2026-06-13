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
//! # Inbound vs outbound symmetry
//!
//! The two keyed forms cover the two directions of per-peer flow control:
//!
//! - **Inbound** is the [`KeyedRateLimiter`]: a remote peer drives requests at
//!   us and we admit or refuse each one against the quota we grant that peer.
//!   The decision is synchronous; a refused request is simply rejected.
//! - **Outbound** is the [`SelfRateLimiter`]: we drive requests at a remote
//!   peer over an accounting-gated protocol (a request consumes the credit the
//!   remote extends us via accounting), so issuing faster than the remote
//!   replenishes our allowance wastes round trips on refusals. Instead of
//!   dropping a request that the bucket cannot admit yet, the self-limiter
//!   parks it on a delay queue and surfaces it again once the bucket has
//!   refilled, throttling our own send rate to stay under the allowance.
//!
//! Both wrap the same GCRA bucket; the self-limiter adds the parking queue and
//! the timer that wakes parked requests. See [`SelfRateLimiter`] for the
//! outbound API.
//!
//! [`ConnectionHandler`]: https://docs.rs/libp2p/0.56/libp2p/swarm/trait.ConnectionHandler.html

mod self_limiter;

pub use self_limiter::{DelayUntil, SelfRateLimiter};

use parking_lot::Mutex;
use std::collections::HashMap;
use std::hash::Hash;
use std::num::NonZeroU32;
use std::time::Duration;

use vertex_util_runtime::time::Instant;

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

/// Per-key GCRA state: the theoretical arrival time plus the [`Cell`] that
/// shapes this key's bucket. The cell is normally the limiter-wide default, but
/// a key whose quota was set through [`KeyedRateLimiter::set_key_quota`] carries
/// its own.
#[derive(Clone, Copy)]
struct KeyState {
    tat_nanos: u64,
    cell: Cell,
}

/// A [`RateLimiter`] per key, shareable via `&self` through an internal mutex.
/// Buckets are lazily inserted on first use; call [`Self::clear`] to release
/// them on peer disconnect.
///
/// # Per-key quotas
///
/// Every key uses the limiter-wide quota passed to [`Self::new`] until
/// [`Self::set_key_quota`] gives that key its own. This lets an outbound
/// throttle resize one peer's bucket from a per-peer signal (a negotiated
/// allowance) without disturbing the others, which is what
/// [`SelfRateLimiter::set_quota`] drives.
pub struct KeyedRateLimiter<K: Eq + Hash> {
    cell: Cell,
    init: Instant,
    state_per_key: Mutex<HashMap<K, KeyState>>,
}

impl<K: Eq + Hash> KeyedRateLimiter<K> {
    pub fn new(quota: Quota) -> Self {
        Self {
            cell: Cell::from_quota(quota),
            init: Instant::now(),
            state_per_key: Mutex::new(HashMap::new()),
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
        let default_cell = self.cell;
        let mut guard = self.state_per_key.lock();
        let state = guard.entry(key).or_insert(KeyState {
            tat_nanos: 0,
            cell: default_cell,
        });
        match check(&state.cell, state.tat_nanos, now, n) {
            Ok(new_tat) => {
                state.tat_nanos = new_tat;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Check whether charging `n` tokens against `key` would be admitted, without
    /// mutating the bucket.
    ///
    /// Returns the same [`RateLimitedErr`] a charge would, so callers can read
    /// the wait hint for a key (used by [`SelfRateLimiter`] to arm its delay
    /// timer) without consuming tokens.
    pub fn try_peek(&self, key: K, n: u32) -> Result<(), RateLimitedErr> {
        let now = self.init.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        let guard = self.state_per_key.lock();
        let (cell, tat) = match guard.get(&key) {
            Some(state) => (state.cell, state.tat_nanos),
            None => (self.cell, 0),
        };
        check(&cell, tat, now, n).map(|_| ())
    }

    /// Give `key` its own quota, replacing the limiter-wide default for that key
    /// only.
    ///
    /// The replacement is idempotent and cheap: an unchanged quota leaves the
    /// bucket untouched (so re-applying the same allowance does not reset the
    /// TAT and hand the peer a free burst), while a changed quota re-shapes the
    /// bucket and re-clamps the recorded TAT into the new capacity so a shrink
    /// cannot leave the peer holding more credit than the new bucket permits.
    pub fn set_key_quota(&self, key: K, quota: Quota) {
        let now = self.init.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        let new_cell = Cell::from_quota(quota);
        let mut guard = self.state_per_key.lock();
        match guard.get_mut(&key) {
            Some(state) => {
                if state.cell.tau_nanos == new_cell.tau_nanos
                    && state.cell.t_nanos == new_cell.t_nanos
                {
                    return;
                }
                state.cell = new_cell;
                // Re-clamp: a TAT further than tau ahead of now would mean the
                // bucket owes more than its new capacity, which the GCRA never
                // permits. Clamp so a shrink takes effect immediately.
                let ceiling = now.saturating_add(new_cell.tau_nanos);
                state.tat_nanos = state.tat_nanos.min(ceiling);
            }
            None => {
                guard.insert(
                    key,
                    KeyState {
                        tat_nanos: 0,
                        cell: new_cell,
                    },
                );
            }
        }
    }

    /// The default quota's full-replenish interval, i.e. the window over which
    /// the limiter-wide bucket refills from empty to full. Keys with a per-key
    /// quota refill over their own interval.
    pub fn replenish_all_every(&self) -> Duration {
        Duration::from_nanos(self.cell.tau_nanos)
    }

    /// The full-replenish interval for `key`, honoring a per-key quota if one was
    /// set, else the limiter-wide default.
    pub fn replenish_all_every_for(&self, key: &K) -> Duration {
        let tau = self
            .state_per_key
            .lock()
            .get(key)
            .map(|state| state.cell.tau_nanos)
            .unwrap_or(self.cell.tau_nanos);
        Duration::from_nanos(tau)
    }

    /// Drop the bucket for `key` to release memory; call this on the final
    /// disconnect for that peer.
    pub fn clear(&self, key: &K) {
        self.state_per_key.lock().remove(key);
    }

    /// Drop every bucket whose TAT lies in the past, i.e. that is already
    /// fully replenished. Lighthouse-style periodic cleanup; useful when
    /// disconnect events are not observed (or as a backstop).
    pub fn retain_recent(&self) {
        let now = self.init.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        self.state_per_key
            .lock()
            .retain(|_, state| state.tat_nanos > now);
    }

    pub fn tracked_keys(&self) -> usize {
        self.state_per_key.lock().len()
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
    fn set_key_quota_resizes_only_that_key() {
        // Default is 1/window; widen alice to 3 without touching bob.
        let rl = KeyedRateLimiter::<&'static str>::new(quota_n_per(1, 60));
        rl.set_key_quota("alice", quota_n_per(3, 60));

        assert!(rl.try_consume_n("alice", 3).is_ok());
        assert!(matches!(
            rl.try_consume("alice"),
            Err(RateLimitedErr::TooSoon(_))
        ));

        // Bob keeps the default single-token bucket.
        assert!(rl.try_consume("bob").is_ok());
        assert!(matches!(
            rl.try_consume("bob"),
            Err(RateLimitedErr::TooSoon(_))
        ));
    }

    #[test]
    fn set_key_quota_is_idempotent_no_free_burst() {
        // Drain the bucket, then re-apply the same quota: the peer must not get
        // a fresh burst from a no-op resize.
        let rl = KeyedRateLimiter::<&'static str>::new(quota_n_per(2, 60));
        assert!(rl.try_consume_n("alice", 2).is_ok());
        assert!(matches!(
            rl.try_consume("alice"),
            Err(RateLimitedErr::TooSoon(_))
        ));
        rl.set_key_quota("alice", quota_n_per(2, 60));
        assert!(matches!(
            rl.try_consume("alice"),
            Err(RateLimitedErr::TooSoon(_))
        ));
    }

    #[test]
    fn set_key_quota_shrink_clamps_outstanding_credit() {
        // A wide bucket drained to empty, then shrunk: the smaller bucket must
        // not owe more than its own capacity. After a shrink to 1/window the
        // first charge is still refused (bucket stays empty), never admitted by
        // a stale oversized TAT.
        let rl = KeyedRateLimiter::<&'static str>::new(quota_n_per(10, 60));
        assert!(rl.try_consume_n("alice", 10).is_ok());
        rl.set_key_quota("alice", quota_n_per(1, 60));
        assert!(matches!(
            rl.try_consume("alice"),
            Err(RateLimitedErr::TooSoon(d)) if d <= Duration::from_secs(60)
        ));
    }

    #[test]
    fn set_key_quota_on_unknown_key_creates_bucket() {
        let rl = KeyedRateLimiter::<&'static str>::new(quota_n_per(1, 60));
        rl.set_key_quota("alice", quota_n_per(5, 60));
        assert_eq!(rl.tracked_keys(), 1);
        assert!(rl.try_consume_n("alice", 5).is_ok());
        assert!(matches!(
            rl.try_consume("alice"),
            Err(RateLimitedErr::TooSoon(_))
        ));
    }

    #[test]
    fn replenish_all_every_for_honors_per_key_quota() {
        let rl = KeyedRateLimiter::<&'static str>::new(quota_n_per(1, 1));
        rl.set_key_quota("alice", quota_n_per(1, 7));
        assert_eq!(rl.replenish_all_every_for(&"alice"), Duration::from_secs(7));
        // Unknown keys fall back to the limiter-wide window.
        assert_eq!(rl.replenish_all_every_for(&"bob"), Duration::from_secs(1));
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
