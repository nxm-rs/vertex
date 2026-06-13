//! Outbound self-throttle on top of a [`KeyedRateLimiter`].
//!
//! [`SelfRateLimiter`] is the outbound dual of [`KeyedRateLimiter`]: instead of
//! admitting or refusing a remote peer's inbound requests, it throttles the
//! requests we send to a remote peer over an accounting-gated protocol so we
//! stay under the credit that peer extends us. A request the bucket cannot
//! admit yet is parked on an internal delay queue and surfaced again once the
//! bucket has refilled, rather than being issued and refused.
//!
//! [`KeyedRateLimiter`]: crate::KeyedRateLimiter

use std::collections::HashMap;
use std::collections::VecDeque;
use std::collections::hash_map::Entry;
use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures_timer::Delay;

use crate::{KeyedRateLimiter, Quota, RateLimitedErr};

/// The wait hint returned when a charge cannot be admitted yet.
///
/// Carries the [`Duration`] from the GCRA bucket after which a retry of the
/// same charge would be admitted. The caller can either schedule its own retry
/// or hand the request to [`SelfRateLimiter::enqueue`] to have the limiter park
/// and drain it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelayUntil(pub Duration);

impl DelayUntil {
    /// The wait hint after which the charge would be admitted.
    pub fn duration(self) -> Duration {
        self.0
    }
}

/// One parked request: the value to surface and the cost to charge when it is.
struct Item<T> {
    cost: u32,
    value: T,
}

/// A FIFO of requests parked behind one key's bucket, plus the timer that wakes
/// the head. The timer is armed to the earliest moment the head item could be
/// admitted and re-armed whenever the head changes.
struct Parked<T> {
    queue: VecDeque<Item<T>>,
    timer: Delay,
}

/// Outbound self-rate-limiter keyed by `K` (typically a `PeerId`).
///
/// Wraps a [`KeyedRateLimiter`] and adds a per-key parking queue so requests
/// the bucket cannot admit yet are delayed rather than refused.
///
/// # Driving the queue
///
/// Drive draining through [`Self::poll_ready`] (or the convenience
/// [`Self::next_ready`] future): poll it to receive `(K, T)` pairs whose bucket has
/// refilled, in per-key enqueue order. Each yielded item has already had its
/// cost charged against the bucket, so the caller can issue it immediately.
///
/// # Cost function
///
/// The cost (token count) is supplied per call, so the consuming protocol crate
/// derives it from request size or kind however it likes.
///
/// # Cleanup
///
/// Call [`Self::clear`] on peer disconnect to drop the bucket and any parked
/// requests for that key, mirroring [`KeyedRateLimiter::clear`].
pub struct SelfRateLimiter<K: Eq + Hash + Clone, T = ()> {
    limiter: KeyedRateLimiter<K>,
    parked: HashMap<K, Parked<T>>,
}

impl<K: Eq + Hash + Clone, T> SelfRateLimiter<K, T> {
    /// Build a self-limiter whose per-key buckets follow `quota`.
    pub fn new(quota: Quota) -> Self {
        Self {
            limiter: KeyedRateLimiter::new(quota),
            parked: HashMap::new(),
        }
    }

    /// Try to charge `cost` against `key` without parking.
    ///
    /// Returns `Ok(())` when the bucket had room (the cost is consumed), or
    /// `Err(DelayUntil(d))` when the bucket is empty (`d` is the earliest a
    /// retry would succeed). A cost larger than the bucket capacity also yields
    /// an `Err`, whose duration is the full replenish interval; such a charge
    /// can never be admitted, so callers that can vary cost should reduce it.
    ///
    /// This does not enqueue the request; use [`Self::enqueue`] to have the
    /// limiter park and later surface it.
    pub fn try_send(&self, key: K, cost: u32) -> Result<(), DelayUntil> {
        match self.limiter.try_consume_n(key.clone(), cost) {
            Ok(()) => Ok(()),
            Err(RateLimitedErr::TooSoon(d)) => Err(DelayUntil(d)),
            Err(RateLimitedErr::TooLarge) => {
                Err(DelayUntil(self.limiter.replenish_all_every_for(&key)))
            }
        }
    }

    /// Charge `cost` against `key`, parking the carried `value` if the bucket is
    /// not ready.
    ///
    /// On success the value is admitted immediately and returned as
    /// `Ok(Some(value))`; the caller issues it right away. When the bucket is
    /// not ready the value is appended to the key's FIFO and `Ok(None)` is
    /// returned; it will later surface from [`Self::poll_ready`] once the bucket
    /// has refilled. `Err(value)` hands the value back when the cost exceeds the
    /// bucket capacity and so can never be admitted.
    pub fn enqueue(&mut self, key: K, cost: u32, value: T) -> Result<Option<T>, T> {
        if self.cost_exceeds_capacity(key.clone(), cost) {
            return Err(value);
        }

        match self.parked.entry(key.clone()) {
            // Items already parked for this key: park behind them to preserve
            // FIFO order rather than letting a fresh charge jump the queue. The
            // drain loop charges this item against the bucket when its turn
            // comes.
            Entry::Occupied(mut e) => {
                e.get_mut().queue.push_back(Item { cost, value });
                Ok(None)
            }
            Entry::Vacant(e) => match self.limiter.try_consume_n(key, cost) {
                Ok(()) => Ok(Some(value)),
                Err(RateLimitedErr::TooSoon(d)) => {
                    let mut queue = VecDeque::new();
                    queue.push_back(Item { cost, value });
                    e.insert(Parked {
                        queue,
                        timer: Delay::new(d),
                    });
                    Ok(None)
                }
                // Re-checked above, so unreachable in practice; fail safe.
                Err(RateLimitedErr::TooLarge) => Err(value),
            },
        }
    }

    fn cost_exceeds_capacity(&self, key: K, cost: u32) -> bool {
        matches!(
            self.limiter.try_peek(key, cost),
            Err(RateLimitedErr::TooLarge)
        )
    }

    /// Resize `key`'s bucket to follow `quota`, replacing the limiter-wide
    /// default for that key only.
    ///
    /// This is how an outbound throttle tracks a per-peer signal (a negotiated
    /// allowance): when the signal changes, call this with the new quota and the
    /// peer's bucket re-shapes promptly. Re-applying the same quota is a no-op,
    /// so a steady allowance does not hand the peer a fresh burst, and a shrink
    /// re-clamps any outstanding credit into the smaller bucket immediately.
    ///
    /// Already-parked items keep their recorded cost; the next drain re-checks
    /// them against the resized bucket, so a shrink simply makes them wait
    /// longer rather than admitting them early.
    pub fn set_quota(&self, key: K, quota: Quota) {
        self.limiter.set_key_quota(key, quota);
    }

    /// Drop the bucket and any parked items for `key`.
    ///
    /// Mirrors [`KeyedRateLimiter::clear`]; call on the final disconnect for the
    /// peer so memory does not grow with the count of distinct peers seen.
    pub fn clear(&mut self, key: &K) {
        self.parked.remove(key);
        self.limiter.clear(key);
    }

    /// Number of keys with at least one parked item. Test and metrics helper.
    pub fn parked_keys(&self) -> usize {
        self.parked.len()
    }

    /// Poll for the next parked item whose bucket has refilled.
    ///
    /// Returns `Poll::Ready((key, value))` for an item that was parked and is
    /// now admitted (its cost is charged before it is yielded). Returns
    /// `Poll::Pending` while items remain parked but none are ready; the context
    /// is woken when the earliest timer fires. When nothing is parked it returns
    /// `Poll::Pending` without registering a waker, so callers must re-poll
    /// after an [`Self::enqueue`].
    pub fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<(K, T)> {
        // Find a key whose head timer has elapsed. Polling every key's timer
        // also (re-)registers the waker on the ones still pending.
        let ready_key = self.parked.iter_mut().find_map(|(key, parked)| {
            match Pin::new(&mut parked.timer).poll(cx) {
                Poll::Ready(()) => Some(key.clone()),
                Poll::Pending => None,
            }
        });

        let Some(key) = ready_key else {
            return Poll::Pending;
        };

        let cost = match self.parked.get(&key).and_then(|p| p.queue.front()) {
            Some(item) => item.cost,
            None => {
                // Empty queue should not be retained; drop and re-poll.
                self.parked.remove(&key);
                return Poll::Pending;
            }
        };

        // The timer fired on the wait hint we recorded, so this charge should
        // succeed; re-check against the live bucket to stay correct under clock
        // skew or a concurrent charge through the wrapped limiter.
        match self.limiter.try_consume_n(key.clone(), cost) {
            Ok(()) => {
                match self.parked.get_mut(&key).and_then(|p| p.queue.pop_front()) {
                    Some(item) => {
                        self.rearm_or_remove(&key, cx);
                        Poll::Ready((key, item.value))
                    }
                    // The head was read above under the same `&mut self`, so it
                    // is still present; refund the charge and re-poll if not.
                    None => {
                        self.parked.remove(&key);
                        Poll::Pending
                    }
                }
            }
            Err(RateLimitedErr::TooSoon(d)) => {
                // Lost a race; re-arm the head timer and wait again.
                if let Some(parked) = self.parked.get_mut(&key) {
                    parked.timer.reset(d);
                    let _ = Pin::new(&mut parked.timer).poll(cx);
                }
                Poll::Pending
            }
            Err(RateLimitedErr::TooLarge) => {
                // Cost can never be admitted; drop the head and continue.
                if let Some(parked) = self.parked.get_mut(&key) {
                    parked.queue.pop_front();
                }
                self.rearm_or_remove(&key, cx);
                Poll::Pending
            }
        }
    }

    /// Re-arm the head timer for `key`, or drop the key when its queue drained.
    fn rearm_or_remove(&mut self, key: &K, cx: &mut Context<'_>) {
        let next_cost = match self.parked.get(key) {
            Some(parked) => match parked.queue.front() {
                Some(item) => item.cost,
                None => {
                    self.parked.remove(key);
                    return;
                }
            },
            None => return,
        };
        // Arm against the live bucket so the next head fires exactly when it can
        // be admitted (zero if already ready).
        let wait = match self.limiter.try_peek(key.clone(), next_cost) {
            Err(RateLimitedErr::TooSoon(d)) => d,
            _ => Duration::ZERO,
        };
        if let Some(parked) = self.parked.get_mut(key) {
            parked.timer.reset(wait);
            let _ = Pin::new(&mut parked.timer).poll(cx);
        }
    }

    /// Future that resolves with the next ready parked item. Convenience over
    /// [`Self::poll_ready`] for `async` call sites.
    pub fn next_ready(&mut self) -> Next<'_, K, T> {
        Next { inner: self }
    }
}

/// Future returned by [`SelfRateLimiter::next_ready`].
pub struct Next<'a, K: Eq + Hash + Clone, T> {
    inner: &'a mut SelfRateLimiter<K, T>,
}

impl<K: Eq + Hash + Clone, T> Future for Next<'_, K, T> {
    type Output = (K, T);

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().inner.poll_ready(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU32;
    use std::time::Duration;

    fn quota_n_per(n: u32, millis: u64) -> Quota {
        Quota::n_every(NonZeroU32::new(n).unwrap(), Duration::from_millis(millis))
    }

    #[test]
    fn under_budget_sends_immediately() {
        let rl = SelfRateLimiter::<&'static str>::new(quota_n_per(3, 60_000));
        assert_eq!(rl.try_send("alice", 1), Ok(()));
        assert_eq!(rl.try_send("alice", 1), Ok(()));
        assert_eq!(rl.try_send("alice", 1), Ok(()));
    }

    #[test]
    fn over_budget_returns_delay() {
        let rl = SelfRateLimiter::<&'static str>::new(quota_n_per(1, 60_000));
        assert_eq!(rl.try_send("alice", 1), Ok(()));
        match rl.try_send("alice", 1) {
            Err(DelayUntil(d)) => {
                assert!(d > Duration::ZERO);
                assert!(d <= Duration::from_millis(60_000));
            }
            other => panic!("expected a delay, got {other:?}"),
        }
    }

    #[test]
    fn oversized_cost_reports_delay() {
        let rl = SelfRateLimiter::<&'static str>::new(quota_n_per(2, 1_000));
        // Cost exceeds capacity; reported as the full replenish window.
        assert_eq!(
            rl.try_send("alice", 3),
            Err(DelayUntil(Duration::from_millis(1_000)))
        );
    }

    #[test]
    fn per_key_independence() {
        let rl = SelfRateLimiter::<&'static str>::new(quota_n_per(1, 60_000));
        assert_eq!(rl.try_send("alice", 1), Ok(()));
        assert!(rl.try_send("alice", 1).is_err());
        // Bob has his own bucket.
        assert_eq!(rl.try_send("bob", 1), Ok(()));
    }

    #[test]
    fn enqueue_admits_under_budget_without_parking() {
        let mut rl = SelfRateLimiter::<&'static str, u32>::new(quota_n_per(2, 60_000));
        assert_eq!(rl.enqueue("alice", 1, 10), Ok(Some(10)));
        assert_eq!(rl.parked_keys(), 0);
    }

    #[test]
    fn enqueue_parks_when_over_budget() {
        let mut rl = SelfRateLimiter::<&'static str, u32>::new(quota_n_per(1, 60_000));
        assert_eq!(rl.enqueue("alice", 1, 10), Ok(Some(10)));
        // Second item cannot be admitted now; it is parked.
        assert_eq!(rl.enqueue("alice", 1, 20), Ok(None));
        assert_eq!(rl.parked_keys(), 1);
    }

    #[test]
    fn enqueue_oversized_cost_returns_value() {
        let mut rl = SelfRateLimiter::<&'static str, u32>::new(quota_n_per(2, 60_000));
        assert_eq!(rl.enqueue("alice", 3, 99), Err(99));
        assert_eq!(rl.parked_keys(), 0);
    }

    #[test]
    fn set_quota_widens_one_peers_bucket() {
        let rl = SelfRateLimiter::<&'static str>::new(quota_n_per(1, 60_000));
        rl.set_quota("alice", quota_n_per(3, 60_000));
        assert_eq!(rl.try_send("alice", 3), Ok(()));
        assert!(rl.try_send("alice", 1).is_err());
        // Bob keeps the default single-token bucket.
        assert_eq!(rl.try_send("bob", 1), Ok(()));
        assert!(rl.try_send("bob", 1).is_err());
    }

    #[test]
    fn set_quota_shrink_throttles_immediately() {
        // A wide bucket is drained, then the allowance shrinks: the next send
        // must be refused, not admitted from stale oversized credit.
        let rl = SelfRateLimiter::<&'static str>::new(quota_n_per(10, 60_000));
        assert_eq!(rl.try_send("alice", 10), Ok(()));
        rl.set_quota("alice", quota_n_per(1, 60_000));
        assert!(rl.try_send("alice", 1).is_err());
    }

    #[test]
    fn try_send_too_large_reports_per_key_window() {
        // A peer with a tiny per-key bucket reports its own replenish window for
        // an over-capacity cost, not the limiter-wide default.
        let rl = SelfRateLimiter::<&'static str>::new(quota_n_per(2, 1_000));
        rl.set_quota("alice", quota_n_per(1, 5_000));
        assert_eq!(
            rl.try_send("alice", 2),
            Err(DelayUntil(Duration::from_millis(5_000)))
        );
    }

    #[test]
    fn clear_releases_key_and_parked_items() {
        let mut rl = SelfRateLimiter::<&'static str, u32>::new(quota_n_per(1, 60_000));
        assert_eq!(rl.enqueue("alice", 1, 1), Ok(Some(1)));
        assert_eq!(rl.enqueue("alice", 1, 2), Ok(None));
        assert_eq!(rl.parked_keys(), 1);
        rl.clear(&"alice");
        assert_eq!(rl.parked_keys(), 0);
        // Bucket is fresh after clear, so a new charge is admitted immediately.
        assert_eq!(rl.try_send("alice", 1), Ok(()));
    }

    #[tokio::test]
    async fn parked_item_drains_after_replenish() {
        // One token per 30 ms: the parked item becomes ready a short, bounded
        // wait later. A real timer drives this (futures-timer), so the assertion
        // is "becomes ready within a generous bound" rather than an exact tick.
        let mut rl = SelfRateLimiter::<&'static str, u32>::new(quota_n_per(1, 30));
        assert_eq!(rl.enqueue("alice", 1, 10), Ok(Some(10)));
        assert_eq!(rl.enqueue("alice", 1, 20), Ok(None));

        let drained = tokio::time::timeout(Duration::from_secs(5), rl.next_ready())
            .await
            .expect("parked item should drain before the timeout");
        assert_eq!(drained, ("alice", 20));
        assert_eq!(rl.parked_keys(), 0);
    }

    #[tokio::test]
    async fn parked_items_drain_in_fifo_order() {
        let mut rl = SelfRateLimiter::<&'static str, u32>::new(quota_n_per(1, 25));
        // First admitted immediately, next two parked behind it.
        assert_eq!(rl.enqueue("alice", 1, 1), Ok(Some(1)));
        assert_eq!(rl.enqueue("alice", 1, 2), Ok(None));
        assert_eq!(rl.enqueue("alice", 1, 3), Ok(None));

        let first = tokio::time::timeout(Duration::from_secs(5), rl.next_ready())
            .await
            .expect("first parked item drains");
        assert_eq!(first, ("alice", 2));

        let second = tokio::time::timeout(Duration::from_secs(5), rl.next_ready())
            .await
            .expect("second parked item drains");
        assert_eq!(second, ("alice", 3));
        assert_eq!(rl.parked_keys(), 0);
    }

    #[tokio::test]
    async fn poll_ready_is_pending_when_nothing_parked() {
        let mut rl = SelfRateLimiter::<&'static str, u32>::new(quota_n_per(1, 60_000));
        let mut fut = rl.next_ready();
        let mut fut = Pin::new(&mut fut);
        std::future::poll_fn(|cx| {
            assert!(fut.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }
}
