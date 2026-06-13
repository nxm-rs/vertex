//! Outbound self-throttle for the accounting-gated chunk-transfer protocols.
//!
//! Pushsync and retrieval both consume the credit a remote peer extends us
//! through pseudosettle: every chunk we push or request draws down the
//! time-based allowance that peer grants. Issue requests faster than the peer
//! replenishes that allowance and the peer refuses to serve us, so the request
//! was a wasted round trip and, if we keep it up, the peer trips its
//! refuse-or-disconnect threshold and drops us.
//!
//! [`SelfThrottle`] paces our own outbound rate to stay under that allowance. It
//! is the single seam both protocols share: one [`SelfRateLimiter`] keyed by the
//! peer's overlay, fed by one per-peer allowance signal
//! ([`PeerAffordability::allowance_remaining`], built once in the accounting
//! layer), so the two protocols cannot each pace against a private, divergent
//! view of the same allowance.
//!
//! # Token model
//!
//! Tokens are *settle units*. One token is `settle_unit_size` accounting units
//! (AU), where `settle_unit_size` is the pseudosettle per-second forgiveness
//! rate (`refresh_rate` AU per second times one second). That makes the bucket
//! refill at exactly one token per second, matching the rate at which the remote
//! forgives our debt, and a peer's bucket capacity is its remaining allowance
//! expressed in those settle units. Working in settle units keeps the GCRA's
//! `u32` token counts well clear of the raw AU range, which spans millions.
//!
//! # Cost per request
//!
//! - **Pushsync**: `ceil(chunk_cost / settle_unit_size)`, where `chunk_cost` is
//!   the representative AU price of a maximal chunk delivery. A pushed chunk has
//!   a fixed maximal footprint, so the cost is a per-protocol constant.
//! - **Retrieval**: a flat `ceil(max_chunk_cost / settle_unit_size)`. We do not
//!   know the response size before it arrives, so we charge the maximal-chunk
//!   estimate up front. TODO(#132): refine once response-size measurements make
//!   a tighter estimate safe.

use std::sync::Arc;

use futures_timer::Delay;
use parking_lot::Mutex;
use vertex_net_ratelimiter::{Quota, SelfRateLimiter};
use vertex_swarm_api::{Au, PeerAffordability};
use vertex_swarm_primitives::OverlayAddress;

use std::num::NonZeroU32;
use std::time::Duration;

/// Which protocol is asking, so the throttle charges the right cost and emits
/// the right metric. The string is the metric prefix and round-trips through
/// the `peer_overlay` label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProtocolKind {
    /// Outbound pushsync delivery.
    Pushsync,
    /// Outbound retrieval request.
    Retrieval,
}

impl ProtocolKind {
    /// The `_total` counter name incremented when a request of this kind is
    /// delayed by the throttle.
    fn throttled_metric(self) -> &'static str {
        match self {
            ProtocolKind::Pushsync => "pushsync_self_throttled_total",
            ProtocolKind::Retrieval => "retrieval_self_throttled_total",
        }
    }
}

/// Upper bound retries before a single throttle wait gives up and admits the
/// request anyway.
///
/// Each loop iteration waits out the bucket's own wait hint, so reaching this
/// bound means the live allowance kept shrinking faster than the request could
/// drain. The request is then let through rather than parked forever: the remote
/// may refuse it, but a refused request is recoverable, whereas a stuck future
/// is not. In practice a steady or growing allowance admits well within one or
/// two iterations.
const MAX_THROTTLE_ITERATIONS: usize = 64;

/// Paces outbound pushsync and retrieval requests under the remote peer's
/// pseudosettle allowance.
///
/// Cheap to clone-by-`Arc`; one instance is shared by both protocols through the
/// client handle.
pub struct SelfThrottle {
    /// Per-peer GCRA buckets keyed by overlay, behind a mutex so the throttle is
    /// `&self`-shareable from the async outbound API. The limiter's own delay
    /// queue is unused here: the per-request async model waits on the bucket's
    /// wait hint directly, which fits the substream-as-correlation request model
    /// without a central poller.
    limiter: Mutex<SelfRateLimiter<OverlayAddress>>,
    /// The per-peer allowance signal, built once in the accounting layer. Both
    /// protocols resize their shared bucket from this same source.
    allowance: Arc<dyn PeerAffordability>,
    /// AU forgiven per second; one bucket token is this many AU. Always at least
    /// one so the division that derives token counts cannot divide by zero.
    settle_unit_size: u64,
    /// Fixed pushsync cost in tokens (`ceil(chunk_cost / settle_unit_size)`).
    pushsync_cost: u32,
    /// Fixed retrieval cost in tokens (`ceil(max_chunk_cost / settle_unit_size)`).
    retrieval_cost: u32,
}

impl SelfThrottle {
    /// Build a throttle from the per-peer allowance signal, the settle-unit size
    /// (`refresh_rate` AU per second), and the representative AU cost of a
    /// maximal chunk.
    ///
    /// `settle_unit_size` and `max_chunk_cost` are clamped to at least one AU so
    /// a misconfigured zero rate can never panic on a divide and never yields a
    /// zero-cost (free, unthrottled) request.
    pub fn new(
        allowance: Arc<dyn PeerAffordability>,
        settle_unit_size: Au,
        max_chunk_cost: Au,
    ) -> Self {
        let settle_unit_size = settle_unit_size.as_amount().max(1);
        let chunk_cost = max_chunk_cost.as_amount().max(1);
        let cost = cost_in_tokens(chunk_cost, settle_unit_size);

        // The default quota only seeds a bucket before the first allowance sync;
        // `set_quota` immediately re-sizes it from the live allowance. One token
        // per second matches the forgiveness rate.
        let default_quota = Quota::n_every(NonZeroU32::MIN, Duration::from_secs(1));

        Self {
            limiter: Mutex::new(SelfRateLimiter::new(default_quota)),
            allowance,
            settle_unit_size,
            // Pushsync and retrieval both charge a maximal-chunk estimate today;
            // they are separate fields so pushsync can later vary by actual chunk
            // size without disturbing retrieval's flat estimate.
            pushsync_cost: cost,
            retrieval_cost: cost,
        }
    }

    /// Re-size `peer`'s bucket to its current allowance, returning the cost the
    /// caller's protocol should charge.
    ///
    /// The bucket capacity is the remaining allowance in settle units, refilling
    /// at one token per second. A zero allowance still yields a one-token bucket
    /// so the GCRA quota stays valid; such a peer is throttled hard but never
    /// divides by zero.
    fn sync_quota(&self, peer: &OverlayAddress, kind: ProtocolKind) -> u32 {
        let allowance = self.allowance.allowance_remaining(peer).as_amount();
        let capacity = cost_in_tokens(allowance, self.settle_unit_size).max(1);
        // Burst = capacity tokens; refill = 1 token/sec (capacity tokens over
        // capacity seconds). This is the pseudosettle forgiveness rate.
        let quota = Quota::n_every(
            NonZeroU32::new(capacity).unwrap_or(NonZeroU32::MIN),
            Duration::from_secs(u64::from(capacity)),
        );
        self.limiter.lock().set_quota(*peer, quota);
        match kind {
            ProtocolKind::Pushsync => self.pushsync_cost,
            ProtocolKind::Retrieval => self.retrieval_cost,
        }
    }

    /// Wait until the peer's bucket admits one request of `kind`, then return.
    ///
    /// Returns immediately when the bucket has room. Otherwise it sleeps the
    /// bucket's own wait hint and retries, re-syncing the live allowance each
    /// iteration so a growing allowance shortens the wait and a shrinking one
    /// lengthens it. The first delay increments the per-peer throttle metric.
    pub(crate) async fn acquire(&self, peer: OverlayAddress, kind: ProtocolKind) {
        let mut throttled = false;
        for _ in 0..MAX_THROTTLE_ITERATIONS {
            let cost = self.sync_quota(&peer, kind);
            let decision = self.limiter.lock().try_send(peer, cost);
            match decision {
                Ok(()) => return,
                Err(delay) => {
                    if !throttled {
                        throttled = true;
                        metrics::counter!(
                            kind.throttled_metric(),
                            "peer_overlay" => peer.to_string(),
                        )
                        .increment(1);
                    }
                    Delay::new(delay.duration()).await;
                }
            }
        }
        // Iteration budget exhausted: let the request through. The remote may
        // refuse it, but that is recoverable; a future that never resolves is
        // not. This only happens under a persistently collapsing allowance.
        tracing::debug!(
            %peer,
            ?kind,
            "self-throttle iteration budget exhausted; releasing request"
        );
    }

    /// Drop the peer's bucket on disconnect so memory does not grow with the
    /// count of distinct peers seen, and a later reconnect starts from a fresh
    /// allowance rather than stale credit.
    pub fn clear(&self, peer: &OverlayAddress) {
        self.limiter.lock().clear(peer);
    }
}

/// `ceil(amount / unit)` clamped into `u32`.
///
/// Both arguments are non-zero AU amounts. The result is the token count for an
/// `amount` of AU at `unit` AU per token; a partial token rounds up so a request
/// is never charged less than it costs. An amount that would overflow `u32`
/// saturates, which only makes the throttle more conservative.
fn cost_in_tokens(amount: u64, unit: u64) -> u32 {
    let unit = unit.max(1);
    let tokens = amount.div_ceil(unit);
    u32::try_from(tokens).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn peer(n: u8) -> OverlayAddress {
        OverlayAddress::from([n; 32])
    }

    /// Allowance source whose value is swappable at runtime, so tests can model
    /// the accounting layer raising or collapsing a peer's allowance.
    struct DynamicAllowance(AtomicU64);

    impl DynamicAllowance {
        fn new(initial: u64) -> Arc<Self> {
            Arc::new(Self(AtomicU64::new(initial)))
        }
        fn set(&self, value: u64) {
            self.0.store(value, Ordering::SeqCst);
        }
    }

    impl PeerAffordability for DynamicAllowance {
        fn can_afford(&self, _overlay: &OverlayAddress, price: Au) -> bool {
            price.as_amount() <= self.0.load(Ordering::SeqCst)
        }
        fn allowance_remaining(&self, _overlay: &OverlayAddress) -> Au {
            Au::from_amount(self.0.load(Ordering::SeqCst))
        }
    }

    fn throttle(allowance: Arc<dyn PeerAffordability>) -> SelfThrottle {
        // settle unit 10 AU; max chunk cost 10 AU => 1 token per request.
        SelfThrottle::new(allowance, Au::from_amount(10), Au::from_amount(10))
    }

    #[test]
    fn cost_rounds_up_partial_tokens() {
        assert_eq!(cost_in_tokens(0, 10), 0);
        assert_eq!(cost_in_tokens(1, 10), 1);
        assert_eq!(cost_in_tokens(10, 10), 1);
        assert_eq!(cost_in_tokens(11, 10), 2);
        // A zero unit never divides by zero.
        assert_eq!(cost_in_tokens(5, 0), 5);
    }

    #[test]
    fn under_allowance_sends_without_delay() {
        // Allowance 100 AU at 10 AU/token => 10-token bucket; one 1-token send
        // is admitted immediately.
        let alloc = DynamicAllowance::new(100);
        let t = throttle(alloc.clone());
        let cost = t.sync_quota(&peer(1), ProtocolKind::Pushsync);
        assert_eq!(cost, 1);
        assert_eq!(t.limiter.lock().try_send(peer(1), cost), Ok(()));
    }

    #[test]
    fn exhausting_allowance_throttles() {
        // 30 AU allowance => 3-token bucket; the fourth 1-token send is refused.
        let alloc = DynamicAllowance::new(30);
        let t = throttle(alloc.clone());
        for _ in 0..3 {
            let cost = t.sync_quota(&peer(1), ProtocolKind::Retrieval);
            assert_eq!(t.limiter.lock().try_send(peer(1), cost), Ok(()));
        }
        let cost = t.sync_quota(&peer(1), ProtocolKind::Retrieval);
        assert!(t.limiter.lock().try_send(peer(1), cost).is_err());
    }

    #[test]
    fn allowance_growth_widens_the_bucket() {
        let alloc = DynamicAllowance::new(10); // 1-token bucket
        let t = throttle(alloc.clone());
        let cost = t.sync_quota(&peer(1), ProtocolKind::Pushsync);
        assert_eq!(t.limiter.lock().try_send(peer(1), cost), Ok(()));
        assert!(t.limiter.lock().try_send(peer(1), cost).is_err());

        // The accounting layer raises the allowance; the next sync resizes the
        // bucket and another send is admitted.
        alloc.set(1000);
        let cost = t.sync_quota(&peer(1), ProtocolKind::Pushsync);
        assert_eq!(t.limiter.lock().try_send(peer(1), cost), Ok(()));
    }

    #[test]
    fn allowance_shrink_throttles_immediately() {
        let alloc = DynamicAllowance::new(1000); // wide bucket
        let t = throttle(alloc.clone());
        // Drain a lot of the wide bucket.
        for _ in 0..50 {
            let cost = t.sync_quota(&peer(1), ProtocolKind::Pushsync);
            assert_eq!(t.limiter.lock().try_send(peer(1), cost), Ok(()));
        }
        // Allowance collapses; the bucket re-clamps and the next send is refused.
        alloc.set(10);
        let cost = t.sync_quota(&peer(1), ProtocolKind::Pushsync);
        assert!(t.limiter.lock().try_send(peer(1), cost).is_err());
    }

    #[test]
    fn clear_drops_peer_bucket() {
        let alloc = DynamicAllowance::new(10);
        let t = throttle(alloc.clone());
        let cost = t.sync_quota(&peer(1), ProtocolKind::Pushsync);
        assert_eq!(t.limiter.lock().try_send(peer(1), cost), Ok(()));
        assert!(t.limiter.lock().try_send(peer(1), cost).is_err());
        // Clearing resets the bucket; a fresh send is admitted.
        t.clear(&peer(1));
        let cost = t.sync_quota(&peer(1), ProtocolKind::Pushsync);
        assert_eq!(t.limiter.lock().try_send(peer(1), cost), Ok(()));
    }

    #[test]
    fn per_peer_buckets_are_independent() {
        let alloc = DynamicAllowance::new(10); // 1-token bucket each
        let t = throttle(alloc.clone());
        let cost = t.sync_quota(&peer(1), ProtocolKind::Pushsync);
        assert_eq!(t.limiter.lock().try_send(peer(1), cost), Ok(()));
        assert!(t.limiter.lock().try_send(peer(1), cost).is_err());
        // A different peer has its own bucket.
        let cost = t.sync_quota(&peer(2), ProtocolKind::Pushsync);
        assert_eq!(t.limiter.lock().try_send(peer(2), cost), Ok(()));
    }

    #[test]
    fn zero_allowance_yields_a_valid_one_token_bucket() {
        // A peer that extends us no allowance must not panic the quota math; it
        // gets the smallest valid bucket and is throttled hard.
        let alloc = DynamicAllowance::new(0);
        let t = throttle(alloc.clone());
        let cost = t.sync_quota(&peer(1), ProtocolKind::Retrieval);
        // First send drains the single token; the second is refused.
        assert_eq!(t.limiter.lock().try_send(peer(1), cost), Ok(()));
        assert!(t.limiter.lock().try_send(peer(1), cost).is_err());
    }

    #[tokio::test]
    async fn acquire_returns_immediately_under_budget() {
        let alloc = DynamicAllowance::new(100);
        let t = throttle(alloc.clone());
        // A generous allowance must resolve promptly; the parking timer
        // (`futures-timer`) does not honor a paused tokio clock, so this is a
        // real but tight bound.
        tokio::time::timeout(
            Duration::from_secs(1),
            t.acquire(peer(1), ProtocolKind::Pushsync),
        )
        .await
        .expect("under-budget acquire resolves at once");
    }

    #[tokio::test]
    async fn acquire_waits_then_admits_after_refill() {
        // 1-token bucket: the first acquire drains it, the second must wait for
        // the bucket to refill (one-second window) and then resolve.
        let alloc = DynamicAllowance::new(10);
        let t = throttle(alloc.clone());
        t.acquire(peer(1), ProtocolKind::Pushsync).await;
        tokio::time::timeout(
            Duration::from_secs(5),
            t.acquire(peer(1), ProtocolKind::Pushsync),
        )
        .await
        .expect("second acquire resolves after refill");
    }
}
