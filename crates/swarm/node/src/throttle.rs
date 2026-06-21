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
//! One token is one accounting unit (AU). A request costs the AU price the
//! remote actually meters for it, and the bucket refills at `refresh_rate` AU
//! per second, matching the rate at which the remote forgives our pseudosettle
//! debt. The throttle therefore paces our outbound rate to exactly the
//! forgiveness rate (hundreds of closest-chunk requests per second, fewer for
//! distant chunks) rather than to a coarse settle-unit granularity. The AU
//! magnitudes in play (a payment threshold of a few million AU, a per-request
//! price in the hundreds of thousands at most) sit comfortably inside the GCRA's
//! `u32` token range.
//!
//! # Bucket capacity and the payment-threshold ceiling
//!
//! A peer's bucket capacity is its live headroom toward the *payment* threshold
//! ([`PeerAffordability::allowance_to_payment_threshold`]), scaled by a
//! configurable safety margin (`throttle_allowance_percent`, default 85%). Two
//! consequences:
//!
//! - The ceiling is the settlement trigger, not the disconnect threshold. The
//!   disconnect threshold sits above the payment threshold, so pacing against
//!   the payment threshold keeps an unpaced post-reconnect burst from crossing
//!   the swap trigger and self-throttling us into paying territory.
//! - The margin leaves headroom below that trigger rather than consuming all of
//!   it, so transient races between the throttle's view and the remote's meter
//!   stay on the safe side.
//!
//! The headroom is live and per-peer. It already includes the credit we earn by
//! serving the same peer: our balance with a peer rises when we provide to them,
//! and that balance feeds the headroom, so a peer we serve heavily gets a wider
//! bucket. Pacing is bilateral and per-peer; there is no cross-peer pooling of
//! credit (that would contradict the bilateral pseudosettle model, where each
//! peer forgives only its own debt).
//!
//! # Cost per request
//!
//! Both pushsync and retrieval charge the exact per-chunk proximity price, the
//! same figure the accounting layer debits when it records the transfer:
//! [`SwarmPricing::peer_price`] of this peer for this chunk address. The price
//! is `base_price * (max_po - proximity + 1)`, highest for a distant chunk and
//! lowest for one in the peer's neighborhood. Retrieval has no separate size
//! term: the network meters a chunk transfer purely by that proximity price, so
//! charging the same price the remote debits keeps the throttle's view of the
//! allowance exactly aligned with the remote's. Charging the real per-chunk
//! price (rather than a flat worst case) is what lets a stream of neighborhood
//! requests pace at the full forgiveness rate instead of being throttled as if
//! every chunk were the most distant possible.

use std::sync::Arc;

use futures_timer::Delay;
use nectar_primitives::ChunkAddress;
use parking_lot::Mutex;
use vertex_net_ratelimiter::{Quota, SelfRateLimiter};
use vertex_swarm_accounting::DefaultBandwidthConfig;
use vertex_swarm_api::{
    PeerAffordability, SwarmAccountingConfig, SwarmClientAccounting, SwarmPricing,
};
use vertex_swarm_primitives::OverlayAddress;

use std::num::NonZeroU32;
use std::time::Duration;

/// Which protocol is asking, so the throttle emits the right metric. The string
/// is the metric prefix and round-trips through the `peer_overlay` label.
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
    /// The chunk pricer, the same instance the accounting layer debits through.
    /// The per-request cost is `peer_price(peer, address)`, so the throttle
    /// charges exactly what the remote meters for the transfer.
    pricing: Arc<dyn SwarmPricing>,
    /// Pseudosettle forgiveness rate in AU per second; the bucket refills at this
    /// many tokens (AU) per second. Always at least one so the quota-window
    /// derivation cannot divide by zero.
    refresh_rate: u64,
    /// Percent (1..=100) of the payment-threshold headroom the bucket is sized
    /// to, leaving a margin below the settlement trigger. Clamped into 1..=100.
    allowance_percent: u64,
}

impl SelfThrottle {
    /// Build a throttle from the client accounting object and the bandwidth
    /// config.
    ///
    /// Everything the throttle paces against comes from these two: the per-peer
    /// allowance signal is `accounting.bandwidth()` (which implements
    /// [`PeerAffordability`]), the chunk pricer (the same one the accounting
    /// layer debits through) is `accounting.pricing()`, the pseudosettle
    /// forgiveness rate (`refresh_rate` AU per second) and the safety-margin
    /// percent of the payment-threshold headroom the bucket is sized to are both
    /// read off `config`.
    ///
    /// `refresh_rate` is clamped to at least one AU so a misconfigured zero rate
    /// can never panic on a divide, and `allowance_percent` is clamped into
    /// 1..=100 so the bucket is never sized to zero or above the live headroom.
    pub fn new<A>(accounting: &A, config: &DefaultBandwidthConfig) -> Self
    where
        A: SwarmClientAccounting,
        A::Bandwidth: PeerAffordability + Clone + 'static,
        A::Pricing: Clone + 'static,
    {
        let allowance: Arc<dyn PeerAffordability> = Arc::new(accounting.bandwidth().clone());
        let pricing: Arc<dyn SwarmPricing> = Arc::new(accounting.pricing().clone());
        let refresh_rate = SwarmAccountingConfig::refresh_rate(config)
            .as_amount()
            .max(1);
        let allowance_percent = u64::from(config.throttle_allowance_percent().clamp(1, 100));

        // The default quota only seeds a bucket before the first allowance sync;
        // `set_quota` immediately re-sizes it from the live allowance. One AU
        // per second is the most conservative possible seed.
        let default_quota = Quota::n_every(NonZeroU32::MIN, Duration::from_secs(1));

        Self {
            limiter: Mutex::new(SelfRateLimiter::new(default_quota)),
            allowance,
            pricing,
            refresh_rate,
            allowance_percent,
        }
    }

    /// Re-size `peer`'s bucket to its current allowance.
    ///
    /// The bucket capacity (burst) is the live headroom toward the payment
    /// threshold scaled by the safety-margin percent, and the bucket refills at
    /// `refresh_rate` AU per second, the pseudosettle forgiveness rate. Sizing to
    /// the payment-threshold headroom (rather than the wider disconnect-threshold
    /// headroom) keeps a burst from crossing the settlement trigger; the margin
    /// leaves slack below it. A zero allowance still yields a one-AU bucket so the
    /// GCRA quota stays valid; such a peer is throttled hard (a one-AU bucket is
    /// smaller than any real per-request cost) but never divides by zero.
    fn sync_quota(&self, peer: &OverlayAddress) {
        let allowance = self
            .allowance
            .allowance_to_payment_threshold(peer)
            .as_amount();
        // Consume only `allowance_percent` of the headroom, leaving a margin
        // below the settlement trigger. The percent is in 1..=100, so the scaled
        // value never exceeds the headroom and the u128 product never overflows.
        let scaled = u128::from(allowance) * u128::from(self.allowance_percent) / 100;
        let capacity = saturating_u32(u64::try_from(scaled).unwrap_or(u64::MAX)).max(1);
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
    }

    /// The exact per-chunk cost the remote meters for serving `address` to us,
    /// in tokens (AU): [`SwarmPricing::peer_price`] of `peer` for `address`,
    /// floored at one AU so a free chunk still draws a token.
    fn request_cost(&self, peer: &OverlayAddress, address: &ChunkAddress) -> u32 {
        saturating_u32(self.pricing.peer_price(peer, address).as_amount().max(1))
    }

    /// Wait until the peer's bucket admits a request for `address`, then return.
    ///
    /// Returns immediately when the bucket has room. Otherwise it sleeps the
    /// bucket's own wait hint and retries, re-syncing the live allowance each
    /// iteration so a growing allowance shortens the wait and a shrinking one
    /// lengthens it. The first delay increments the per-peer throttle metric for
    /// `kind`.
    ///
    /// The cost charged is the exact per-chunk proximity price the remote will
    /// debit ([`SwarmPricing::peer_price`]), so a neighborhood chunk paces at the
    /// full forgiveness rate while a distant one costs proportionally more.
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
    pub(crate) async fn acquire(
        &self,
        peer: OverlayAddress,
        address: ChunkAddress,
        kind: ProtocolKind,
    ) {
        let cost = self.request_cost(&peer, &address);
        let mut throttled = false;
        let mut waited = Duration::ZERO;
        for _ in 0..MAX_THROTTLE_ITERATIONS {
            self.sync_quota(&peer);
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
    use vertex_swarm_api::Au;

    fn peer(n: u8) -> OverlayAddress {
        OverlayAddress::from([n; 32])
    }

    fn address(n: u8) -> ChunkAddress {
        ChunkAddress::from([n; 32])
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

    /// A pricer that returns a fixed per-(peer, chunk) price, modelling the exact
    /// figure the accounting layer would debit. The throttle must charge this.
    struct FixedPrice(u64);

    impl SwarmPricing for FixedPrice {
        fn price(&self, _chunk: &ChunkAddress) -> Au {
            Au::from_amount(self.0)
        }
        fn peer_price(&self, _peer: &OverlayAddress, _chunk: &ChunkAddress) -> Au {
            Au::from_amount(self.0)
        }
    }

    /// Refresh rate used in tests (AU/sec). One token is one AU, so a request
    /// costing `COST` AU drains `COST` of the allowance-sized bucket.
    const REFRESH_RATE: u64 = 10;
    /// Per-request price in AU returned by the test pricer.
    const COST: u64 = 10;

    /// Bandwidth side of the test [`SwarmClientAccounting`] mock.
    ///
    /// Wraps the chosen affordability signal and delegates [`PeerAffordability`]
    /// to it; the [`SwarmBandwidthAccounting`] surface the trait requires is a
    /// no-op since the throttle reads only `bandwidth()` (for affordability) and
    /// `pricing()`.
    #[derive(Clone)]
    struct MockBandwidth(Arc<dyn PeerAffordability>);

    impl PeerAffordability for MockBandwidth {
        fn can_afford(&self, overlay: &OverlayAddress, price: Au) -> bool {
            self.0.can_afford(overlay, price)
        }
        fn allowance_remaining(&self, overlay: &OverlayAddress) -> Au {
            self.0.allowance_remaining(overlay)
        }
        fn allowance_to_payment_threshold(&self, overlay: &OverlayAddress) -> Au {
            self.0.allowance_to_payment_threshold(overlay)
        }
    }

    impl vertex_swarm_api::SwarmBandwidthAccounting for MockBandwidth {
        type Identity = vertex_swarm_test_utils::MockIdentity;
        type Peer = vertex_swarm_accounting::NoPeerBandwidth;
        type ReceiveAction = vertex_swarm_accounting::NoReceiveAction;
        type ProvideAction = vertex_swarm_accounting::NoProvideAction;

        fn identity(&self) -> &Self::Identity {
            unreachable!("throttle never reads the identity")
        }
        fn for_peer(&self, peer: OverlayAddress) -> Self::Peer {
            vertex_swarm_accounting::NoAccounting::new(
                vertex_swarm_test_utils::MockIdentity::with_first_byte(0),
            )
            .for_peer(peer)
        }
        fn peers(&self) -> Vec<OverlayAddress> {
            Vec::new()
        }
        fn remove_peer(&self, _peer: &OverlayAddress) {}
        fn prepare_receive(
            &self,
            _peer: OverlayAddress,
            _price: Au,
            _originated: bool,
        ) -> vertex_swarm_api::SwarmResult<Self::ReceiveAction> {
            Ok(vertex_swarm_accounting::NoReceiveAction)
        }
        fn prepare_provide(
            &self,
            _peer: OverlayAddress,
            _price: Au,
        ) -> vertex_swarm_api::SwarmResult<Self::ProvideAction> {
            Ok(vertex_swarm_accounting::NoProvideAction)
        }
    }

    /// Minimal [`SwarmClientAccounting`] bundling a chosen affordability signal
    /// and pricer so the [`SelfThrottle::new`] ctor can extract both.
    #[derive(Clone)]
    struct MockClientAccounting {
        bandwidth: MockBandwidth,
        pricing: Arc<dyn SwarmPricing>,
    }

    impl SwarmClientAccounting for MockClientAccounting {
        type Bandwidth = MockBandwidth;
        type Pricing = Arc<dyn SwarmPricing>;

        fn bandwidth(&self) -> &Self::Bandwidth {
            &self.bandwidth
        }
        fn pricing(&self) -> &Self::Pricing {
            &self.pricing
        }
    }

    /// Build a throttle from a chosen affordability signal, pricer, refresh rate,
    /// and margin percent, bundling them through the same accounting-object and
    /// config-reference path the production ctor takes.
    fn build_throttle(
        allowance: Arc<dyn PeerAffordability>,
        pricing: Arc<dyn SwarmPricing>,
        refresh_rate: u64,
        allowance_percent: u8,
    ) -> SelfThrottle {
        let accounting = MockClientAccounting {
            bandwidth: MockBandwidth(allowance),
            pricing,
        };
        // Only refresh_rate and throttle_allowance_percent are read off the
        // config; the remaining fields are placeholders that the throttle never
        // touches.
        let config = DefaultBandwidthConfig::new(
            vertex_swarm_api::BandwidthMode::Pseudosettle,
            0,
            0,
            refresh_rate,
            0,
            1,
            allowance_percent,
            Default::default(),
        );
        SelfThrottle::new(&accounting, &config)
    }

    fn throttle(allowance: Arc<dyn PeerAffordability>) -> SelfThrottle {
        // refresh_rate 10 AU/sec; the pricer meters every request at COST AU, so
        // a bucket of N AU admits floor(N / COST) requests. The margin percent is
        // 100 here so the bucket equals the full payment-threshold headroom and
        // the per-request arithmetic stays exact; the margin itself is exercised
        // by `margin_percent_shrinks_the_bucket`.
        build_throttle(allowance, Arc::new(FixedPrice(COST)), REFRESH_RATE, 100)
    }

    #[test]
    fn cost_saturates_into_u32() {
        assert_eq!(saturating_u32(0), 0);
        assert_eq!(saturating_u32(1), 1);
        assert_eq!(saturating_u32(u64::from(u32::MAX)), u32::MAX);
        assert_eq!(saturating_u32(u64::from(u32::MAX) + 1), u32::MAX);
    }

    #[test]
    fn cost_is_the_exact_peer_price() {
        // The per-request cost is the pricer's peer_price, the same figure the
        // accounting layer debits. A 100-AU allowance metered at 10 AU per chunk
        // admits ten requests, not one: this is the regression guard for the
        // over-throttle finding (a flat worst-case cost would admit far fewer).
        let alloc = DynamicAllowance::new(100);
        let t = throttle(alloc.clone());
        let cost = t.request_cost(&peer(1), &address(9));
        assert_eq!(u64::from(cost), COST);
        t.sync_quota(&peer(1));
        for _ in 0..10 {
            assert_eq!(t.limiter.lock().try_send(peer(1), cost), Ok(()));
        }
        assert!(t.limiter.lock().try_send(peer(1), cost).is_err());
    }

    #[test]
    fn cost_floors_at_one_au() {
        // A pricer returning zero must still draw a token so a free chunk is not
        // unthrottled.
        let alloc = DynamicAllowance::new(100);
        let t = build_throttle(alloc, Arc::new(FixedPrice(0)), REFRESH_RATE, 100);
        assert_eq!(t.request_cost(&peer(1), &address(0)), 1);
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
        let cost = t.request_cost(&peer(1), &address(1));
        t.sync_quota(&peer(1));
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
        // 30 AU allowance at a 10-AU price => three requests; the fourth is
        // refused.
        let alloc = DynamicAllowance::new(30);
        let t = throttle(alloc.clone());
        let cost = t.request_cost(&peer(1), &address(1));
        for _ in 0..3 {
            t.sync_quota(&peer(1));
            assert_eq!(t.limiter.lock().try_send(peer(1), cost), Ok(()));
        }
        t.sync_quota(&peer(1));
        assert!(t.limiter.lock().try_send(peer(1), cost).is_err());
    }

    #[test]
    fn allowance_growth_widens_the_bucket() {
        let alloc = DynamicAllowance::new(COST); // one-request bucket
        let t = throttle(alloc.clone());
        let cost = t.request_cost(&peer(1), &address(1));
        t.sync_quota(&peer(1));
        assert_eq!(t.limiter.lock().try_send(peer(1), cost), Ok(()));
        assert!(t.limiter.lock().try_send(peer(1), cost).is_err());

        // The accounting layer raises the allowance; the next sync resizes the
        // bucket and another send is admitted.
        alloc.set(1000);
        t.sync_quota(&peer(1));
        assert_eq!(t.limiter.lock().try_send(peer(1), cost), Ok(()));
    }

    #[test]
    fn allowance_shrink_throttles_immediately() {
        let alloc = DynamicAllowance::new(1000); // wide bucket
        let t = throttle(alloc.clone());
        let cost = t.request_cost(&peer(1), &address(1));
        // Drain a lot of the wide bucket (1000 AU / 10 AU = 100 requests).
        for _ in 0..100 {
            t.sync_quota(&peer(1));
            assert_eq!(t.limiter.lock().try_send(peer(1), cost), Ok(()));
        }
        // Allowance collapses; the bucket re-clamps and the next send is refused.
        alloc.set(COST);
        t.sync_quota(&peer(1));
        assert!(t.limiter.lock().try_send(peer(1), cost).is_err());
    }

    #[test]
    fn clear_drops_peer_bucket() {
        let alloc = DynamicAllowance::new(COST);
        let t = throttle(alloc.clone());
        let cost = t.request_cost(&peer(1), &address(1));
        t.sync_quota(&peer(1));
        assert_eq!(t.limiter.lock().try_send(peer(1), cost), Ok(()));
        assert!(t.limiter.lock().try_send(peer(1), cost).is_err());
        // Clearing resets the bucket; a fresh send is admitted.
        t.clear(&peer(1));
        t.sync_quota(&peer(1));
        assert_eq!(t.limiter.lock().try_send(peer(1), cost), Ok(()));
    }

    #[test]
    fn per_peer_buckets_are_independent() {
        let alloc = DynamicAllowance::new(COST); // one-request bucket each
        let t = throttle(alloc.clone());
        let cost = t.request_cost(&peer(1), &address(1));
        t.sync_quota(&peer(1));
        assert_eq!(t.limiter.lock().try_send(peer(1), cost), Ok(()));
        assert!(t.limiter.lock().try_send(peer(1), cost).is_err());
        // A different peer has its own bucket.
        t.sync_quota(&peer(2));
        assert_eq!(t.limiter.lock().try_send(peer(2), cost), Ok(()));
    }

    #[test]
    fn zero_allowance_yields_a_valid_throttled_bucket() {
        // A peer that extends us no allowance must not panic the quota math; it
        // gets the smallest valid bucket and is throttled hard (capacity 1 AU is
        // smaller than the 10-AU price, so even the first send is refused).
        let alloc = DynamicAllowance::new(0);
        let t = throttle(alloc.clone());
        let cost = t.request_cost(&peer(1), &address(1));
        t.sync_quota(&peer(1));
        assert!(t.limiter.lock().try_send(peer(1), cost).is_err());
    }

    #[test]
    fn margin_percent_shrinks_the_bucket() {
        // With an 80% margin a 100-AU headroom yields an 80-AU bucket, which at a
        // 10-AU price admits eight requests, not ten.
        let alloc = DynamicAllowance::new(100);
        let t = build_throttle(alloc, Arc::new(FixedPrice(COST)), REFRESH_RATE, 80);
        let cost = t.request_cost(&peer(1), &address(1));
        t.sync_quota(&peer(1));
        for _ in 0..8 {
            assert_eq!(t.limiter.lock().try_send(peer(1), cost), Ok(()));
        }
        assert!(t.limiter.lock().try_send(peer(1), cost).is_err());
    }

    #[test]
    fn margin_percent_clamps_into_range() {
        // A zero percent clamps up to 1 (never sizes the bucket to nothing), and a
        // percent above 100 clamps down to 100 (never above the live headroom).
        let alloc = DynamicAllowance::new(1000);
        let lo = build_throttle(alloc.clone(), Arc::new(FixedPrice(COST)), REFRESH_RATE, 0);
        assert_eq!(lo.allowance_percent, 1);
        let hi = build_throttle(alloc, Arc::new(FixedPrice(COST)), REFRESH_RATE, 200);
        assert_eq!(hi.allowance_percent, 100);
    }

    #[test]
    fn paces_against_the_payment_threshold_headroom() {
        // A signal that reports a wide disconnect-threshold headroom but a narrow
        // payment-threshold headroom must be paced by the narrower figure: the
        // throttle stays below the settlement trigger, not the disconnect one.
        struct SplitHeadroom {
            disconnect: u64,
            payment: u64,
        }
        impl PeerAffordability for SplitHeadroom {
            fn can_afford(&self, _overlay: &OverlayAddress, price: Au) -> bool {
                price.as_amount() <= self.disconnect
            }
            fn allowance_remaining(&self, _overlay: &OverlayAddress) -> Au {
                Au::from_amount(self.disconnect)
            }
            fn allowance_to_payment_threshold(&self, _overlay: &OverlayAddress) -> Au {
                Au::from_amount(self.payment)
            }
        }

        // Disconnect headroom is 1000 AU (100 requests) but payment headroom is
        // only 30 AU (three requests at the 10-AU price); margin 100% for an exact
        // count. The fourth request must be refused.
        let t = build_throttle(
            Arc::new(SplitHeadroom {
                disconnect: 1000,
                payment: 30,
            }),
            Arc::new(FixedPrice(COST)),
            REFRESH_RATE,
            100,
        );
        let cost = t.request_cost(&peer(1), &address(1));
        for _ in 0..3 {
            t.sync_quota(&peer(1));
            assert_eq!(t.limiter.lock().try_send(peer(1), cost), Ok(()));
        }
        t.sync_quota(&peer(1));
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
            t.acquire(peer(1), address(1), ProtocolKind::Pushsync),
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
        t.acquire(peer(1), address(1), ProtocolKind::Pushsync).await;
        tokio::time::timeout(
            Duration::from_secs(5),
            t.acquire(peer(1), address(1), ProtocolKind::Pushsync),
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
            t.acquire(peer(1), address(1), ProtocolKind::Pushsync),
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
