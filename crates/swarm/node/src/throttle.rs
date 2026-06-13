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
//! One token is one accounting unit (AU). A peer's bucket capacity is its
//! remaining allowance expressed directly in AU, and the bucket refills at
//! `refresh_rate` AU per second, matching the rate at which the remote forgives
//! our pseudosettle debt. A request costs the AU price the remote actually
//! meters for it, so the throttle paces our outbound rate to exactly the
//! forgiveness rate (hundreds of closest-chunk requests per second, fewer for
//! distant chunks) rather than to a coarse settle-unit granularity. The AU
//! magnitudes in play (a disconnect threshold of a few million AU, a worst-case
//! per-request price in the hundreds of thousands) sit comfortably inside the
//! GCRA's `u32` token range.
//!
//! # Cost per request
//!
//! - **Pushsync**: the representative AU price of a maximal chunk delivery. A
//!   pushed chunk has a fixed maximal footprint, so the cost is a per-protocol
//!   constant.
//! - **Retrieval**: the same flat maximal-chunk price. We do not know the
//!   response size before it arrives, so we charge the maximal-chunk estimate up
//!   front. TODO(#132): refine once response-size measurements make a tighter
//!   estimate safe.
//!
//! The maximal-chunk price is the worst-case (proximity 0) peer price, i.e. the
//! most the remote can debit us for a single chunk. Charging that worst case
//! means the throttle never under-counts a distant chunk against the allowance
//! the remote actually extends.

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

/// Wall-clock cap on a single [`SelfThrottle::acquire`] before it gives up and
/// admits the request anyway.
///
/// The iteration budget alone bounds spins, but a per-iteration wait is only as
/// short as the bucket's refill hint, so a sequence of small waits could still
/// add up. This cap keeps one throttled peer from serialising a candidate walk
/// (a sequential pushsync walk awaits each candidate fully before trying the
/// next): once the cumulative wait reaches it, the request is released so the
/// caller can fall through to the next candidate. Under a steady or growing
/// allowance acquire resolves in well under this bound; it only bites when an
/// allowance is persistently collapsing.
const MAX_THROTTLE_WAIT: Duration = Duration::from_secs(2);

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
    /// Pseudosettle forgiveness rate in AU per second; the bucket refills at this
    /// many tokens (AU) per second. Always at least one so the quota-window
    /// derivation cannot divide by zero.
    refresh_rate: u64,
    /// Fixed pushsync cost in tokens (AU): the worst-case maximal-chunk price.
    pushsync_cost: u32,
    /// Fixed retrieval cost in tokens (AU): the worst-case maximal-chunk price.
    retrieval_cost: u32,
}

impl SelfThrottle {
    /// Build a throttle from the per-peer allowance signal, the pseudosettle
    /// forgiveness rate (`refresh_rate` AU per second), and the worst-case AU
    /// price of a maximal chunk delivery.
    ///
    /// `refresh_rate` and `max_chunk_cost` are clamped to at least one AU so a
    /// misconfigured zero rate can never panic on a divide and never yields a
    /// zero-cost (free, unthrottled) request.
    pub fn new(
        allowance: Arc<dyn PeerAffordability>,
        refresh_rate: Au,
        max_chunk_cost: Au,
    ) -> Self {
        let refresh_rate = refresh_rate.as_amount().max(1);
        let cost = saturating_u32(max_chunk_cost.as_amount().max(1));

        // The default quota only seeds a bucket before the first allowance sync;
        // `set_quota` immediately re-sizes it from the live allowance. One AU
        // per second is the most conservative possible seed.
        let default_quota = Quota::n_every(NonZeroU32::MIN, Duration::from_secs(1));

        Self {
            limiter: Mutex::new(SelfRateLimiter::new(default_quota)),
            allowance,
            refresh_rate,
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
    /// The bucket capacity (burst) is the remaining allowance in AU and the
    /// bucket refills at `refresh_rate` AU per second, the pseudosettle
    /// forgiveness rate. A zero allowance still yields a one-AU bucket so the
    /// GCRA quota stays valid; such a peer is throttled hard (a one-AU bucket is
    /// smaller than any real per-request cost) but never divides by zero.
    fn sync_quota(&self, peer: &OverlayAddress, kind: ProtocolKind) -> u32 {
        let allowance = self.allowance.allowance_remaining(peer).as_amount();
        let capacity = saturating_u32(allowance).max(1);
        // Burst = `capacity` AU; refill = `refresh_rate` AU/sec. The GCRA derives
        // its per-token replenish time as window / capacity, so a window of
        // `capacity / refresh_rate` seconds replenishes one AU every
        // `1 / refresh_rate` seconds regardless of capacity: the refill rate is
        // exactly the forgiveness rate, and the burst is the whole allowance.
        let window_nanos =
            u64::try_from(u128::from(capacity) * 1_000_000_000u128 / u128::from(self.refresh_rate))
                .unwrap_or(u64::MAX)
                .max(1);
        let quota = Quota::n_every(
            NonZeroU32::new(capacity).unwrap_or(NonZeroU32::MIN),
            Duration::from_nanos(window_nanos),
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
    ///
    /// The allowance is read (polled) at each iteration rather than subscribed
    /// to: the bucket is resized on every acquire from the live
    /// [`PeerAffordability`] signal, so a between-request allowance change is
    /// picked up at the next request that paces against the peer. A change while
    /// no request is in flight needs no resize because nothing is being admitted.
    ///
    /// The per-peer `peer_overlay` metric label is deliberately high-cardinality
    /// (the issues require per-peer visibility). Operators with large or churny
    /// peer sets should aggregate it away in a recording rule or rely on metric
    /// TTL; [`Self::clear`] drops a peer's bucket on disconnect but does not (and
    /// cannot) retract an already-emitted counter series.
    pub(crate) async fn acquire(&self, peer: OverlayAddress, kind: ProtocolKind) {
        let mut throttled = false;
        let mut waited = Duration::ZERO;
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
                    // Cap the cumulative wait so one throttled peer cannot
                    // serialise a sequential candidate walk: once the total wait
                    // would exceed the cap, release the request and let the caller
                    // fall through to the next candidate.
                    let wait = delay.duration();
                    if waited.saturating_add(wait) > MAX_THROTTLE_WAIT {
                        break;
                    }
                    waited = waited.saturating_add(wait);
                    Delay::new(wait).await;
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

/// Clamp an AU amount into the GCRA's `u32` token range.
///
/// One token is one AU, so this is the identity on the values the throttle ever
/// sees (allowances of a few million AU, per-request prices in the hundreds of
/// thousands). A value that would overflow `u32` saturates to the maximum, which
/// only widens a bucket or raises a cost: it never under-throttles.
fn saturating_u32(amount: u64) -> u32 {
    u32::try_from(amount).unwrap_or(u32::MAX)
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

    /// Refresh rate used in tests (AU/sec). One token is one AU, so a request
    /// costing `COST` AU drains `COST` of the allowance-sized bucket.
    const REFRESH_RATE: u64 = 10;
    /// Per-request cost in AU for the test throttle.
    const COST: u64 = 10;

    fn throttle(allowance: Arc<dyn PeerAffordability>) -> SelfThrottle {
        // refresh_rate 10 AU/sec; max chunk cost 10 AU => 10-AU (10-token) cost
        // per request, so a bucket of N AU admits floor(N / 10) requests.
        SelfThrottle::new(
            allowance,
            Au::from_amount(REFRESH_RATE),
            Au::from_amount(COST),
        )
    }

    #[test]
    fn cost_saturates_into_u32() {
        assert_eq!(saturating_u32(0), 0);
        assert_eq!(saturating_u32(1), 1);
        assert_eq!(saturating_u32(u64::from(u32::MAX)), u32::MAX);
        assert_eq!(saturating_u32(u64::from(u32::MAX) + 1), u32::MAX);
    }

    #[test]
    fn cost_is_the_au_price_not_a_settle_unit() {
        // The per-request cost is the AU price itself, so a fresh peer's bucket
        // admits allowance / price requests, not allowance / refresh_rate. This
        // is the regression guard for the over-throttle finding: a 100-AU
        // allowance at a 10-AU cost admits ten requests, not one.
        let alloc = DynamicAllowance::new(100);
        let t = throttle(alloc.clone());
        let cost = t.sync_quota(&peer(1), ProtocolKind::Pushsync);
        assert_eq!(u64::from(cost), COST);
        for _ in 0..10 {
            assert_eq!(t.limiter.lock().try_send(peer(1), cost), Ok(()));
        }
        assert!(t.limiter.lock().try_send(peer(1), cost).is_err());
    }

    #[test]
    fn steady_state_pace_matches_forgiveness_rate() {
        // After draining the burst, the bucket refills at refresh_rate AU/sec, so
        // the per-request replenish interval is price / refresh_rate seconds (one
        // second here for COST == REFRESH_RATE). The reported wait hint must
        // reflect that forgiveness-rate pacing, not a hard 1-token-per-second cap
        // that would ignore how generous the allowance is.
        let alloc = DynamicAllowance::new(COST); // exactly one request of burst
        let t = throttle(alloc.clone());
        let cost = t.sync_quota(&peer(1), ProtocolKind::Pushsync);
        assert_eq!(t.limiter.lock().try_send(peer(1), cost), Ok(()));
        match t.limiter.lock().try_send(peer(1), cost) {
            Err(delay) => {
                // price / refresh_rate == 1s; allow generous slack for the GCRA's
                // integer-nanosecond rounding.
                let secs = delay.duration().as_secs_f64();
                assert!(
                    (0.5..=1.5).contains(&secs),
                    "expected ~1s refill, got {secs}s"
                );
            }
            Ok(()) => panic!("second send should be throttled"),
        }
    }

    #[test]
    fn exhausting_allowance_throttles() {
        // 30 AU allowance at a 10-AU cost => three requests; the fourth is
        // refused.
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
        let alloc = DynamicAllowance::new(COST); // one-request bucket
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
        // Drain a lot of the wide bucket (1000 AU / 10 AU = 100 requests).
        for _ in 0..100 {
            let cost = t.sync_quota(&peer(1), ProtocolKind::Pushsync);
            assert_eq!(t.limiter.lock().try_send(peer(1), cost), Ok(()));
        }
        // Allowance collapses; the bucket re-clamps and the next send is refused.
        alloc.set(COST);
        let cost = t.sync_quota(&peer(1), ProtocolKind::Pushsync);
        assert!(t.limiter.lock().try_send(peer(1), cost).is_err());
    }

    #[test]
    fn clear_drops_peer_bucket() {
        let alloc = DynamicAllowance::new(COST);
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
        let alloc = DynamicAllowance::new(COST); // one-request bucket each
        let t = throttle(alloc.clone());
        let cost = t.sync_quota(&peer(1), ProtocolKind::Pushsync);
        assert_eq!(t.limiter.lock().try_send(peer(1), cost), Ok(()));
        assert!(t.limiter.lock().try_send(peer(1), cost).is_err());
        // A different peer has its own bucket.
        let cost = t.sync_quota(&peer(2), ProtocolKind::Pushsync);
        assert_eq!(t.limiter.lock().try_send(peer(2), cost), Ok(()));
    }

    #[test]
    fn zero_allowance_yields_a_valid_throttled_bucket() {
        // A peer that extends us no allowance must not panic the quota math; it
        // gets the smallest valid bucket and is throttled hard (capacity 1 AU is
        // smaller than the 10-AU cost, so even the first send is refused).
        let alloc = DynamicAllowance::new(0);
        let t = throttle(alloc.clone());
        let cost = t.sync_quota(&peer(1), ProtocolKind::Retrieval);
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
        // One-request bucket: the first acquire drains it, the second must wait
        // for the bucket to refill (one-second window for COST == REFRESH_RATE)
        // and then resolve.
        let alloc = DynamicAllowance::new(COST);
        let t = throttle(alloc.clone());
        t.acquire(peer(1), ProtocolKind::Pushsync).await;
        tokio::time::timeout(
            Duration::from_secs(5),
            t.acquire(peer(1), ProtocolKind::Pushsync),
        )
        .await
        .expect("second acquire resolves after refill");
    }

    #[tokio::test]
    async fn acquire_releases_under_collapsing_allowance_within_wall_clock_cap() {
        // A zero allowance can never admit the request: acquire must give up at
        // the wall-clock cap rather than spinning the full iteration budget, so a
        // sequential candidate walk is not serialised by one collapsed peer.
        let alloc = DynamicAllowance::new(0);
        let t = throttle(alloc.clone());
        let start = std::time::Instant::now();
        tokio::time::timeout(
            MAX_THROTTLE_WAIT + Duration::from_secs(2),
            t.acquire(peer(1), ProtocolKind::Pushsync),
        )
        .await
        .expect("acquire releases at the wall-clock cap, not the iteration budget");
        // It must not have parked anywhere near MAX_THROTTLE_ITERATIONS windows.
        assert!(
            start.elapsed() <= MAX_THROTTLE_WAIT + Duration::from_secs(2),
            "acquire parked too long under a collapsed allowance"
        );
    }
}
