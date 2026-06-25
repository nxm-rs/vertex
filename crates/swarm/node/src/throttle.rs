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
//! # Per-peer debt gate
//!
//! The rate bucket bounds the long-run pace, but the load-bearing brake is a hard
//! per-peer debt gate. The remote's only per-peer brake is accounting debt: it
//! resets and drops us once our debt to it (its `balance + shadowReserved` view,
//! taken at request time, ahead of our own delivery debit) crosses its disconnect
//! line. Before admitting a request the throttle reads our live unsettled debt to
//! the peer counted the same way ([`PeerAffordability::unsettled_debt`], committed
//! plus the in-flight reservation) and refuses admission if that debt plus this
//! request's price would cross the peer's payment threshold
//! ([`PeerAffordability::payment_threshold`]). The payment threshold sits below
//! the disconnect line by the payment-tolerance margin, so a gated peer's debt the
//! remote sees stays a margin under the line. The non-blocking path skips a gated
//! peer (the scheduler assigns the chunk elsewhere while a background settle drains
//! the debt); the blocking path settles-and-waits so a candidate walk still makes
//! progress when every peer is momentarily over the ceiling.
//!
//! # Per-peer in-flight cap
//!
//! The allowance pacing alone bounds the long-run *rate* to a peer but not the
//! instantaneous *fan-out*: a file download issues its chunk requests
//! concurrently, and against a fresh peer the allowance bucket admits a large
//! burst before it bites. Too many requests in flight to one peer at once
//! overruns the connection-level stream budget the remote enforces and it resets
//! the streams, churning the connection. A per-peer concurrency permit
//! ([`MAX_INFLIGHT_PER_PEER`]) bounds how many of our requests are outstanding to
//! any single peer, holding the permit for the request's lifetime. The cap is
//! per-peer, not global, so a retrieval race across many peers is unaffected;
//! only the depth piled onto each individual peer is bounded.
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

use std::collections::HashMap;
use std::sync::Arc;

use futures_timer::Delay;
use nectar_primitives::ChunkAddress;
use parking_lot::Mutex;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use vertex_net_ratelimiter::{Quota, SelfRateLimiter};
use vertex_swarm_accounting::DefaultBandwidthConfig;
use vertex_swarm_api::{
    Au, PeerAffordability, SwarmAccountingConfig, SwarmClientAccounting, SwarmPricing,
};
use vertex_swarm_primitives::OverlayAddress;

use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::future::BoxFuture;
use vertex_swarm_api::{SwarmBandwidthAccounting, SwarmPeerBandwidth};
use vertex_tasks::TaskExecutor;
use vertex_tasks::time::Instant;

/// Awaitable debtor-initiated settle for one peer, the pre-pay seam the throttle
/// drives before a request crosses the early-payment trigger.
///
/// Object-safe so the throttle holds it as `Arc<dyn PeerSettle>` without naming
/// the bandwidth accounting's per-peer handle type. The returned future settles
/// the peer's debt through pseudosettle and resolves once the offer is acked (or
/// refused, which is also a resolution). Send (settling is `Send` by the
/// [`SwarmPeerBandwidth::settle`] contract) so the non-blocking path can spawn
/// it on either the native or browser executor. Implemented over any
/// [`SwarmBandwidthAccounting`].
pub(crate) trait PeerSettle: Send + Sync {
    /// Settle `peer`'s outstanding debt, resolving when the offer completes.
    fn settle(&self, peer: OverlayAddress) -> BoxFuture<'static, ()>;
}

impl<B> PeerSettle for B
where
    B: SwarmBandwidthAccounting + 'static,
    B::Peer: 'static,
{
    fn settle(&self, peer: OverlayAddress) -> BoxFuture<'static, ()> {
        let handle = self.for_peer(peer);
        Box::pin(async move {
            if let Err(error) = handle.settle().await {
                tracing::debug!(%peer, %error, "pre-send settle failed");
            }
        })
    }
}

/// Temporary latency-decomposition counters for the retrieval path.
///
/// Splits a retrieval leg's wall time into the part spent inside the admission
/// throttle (allowance pacing) versus the on-wire RTT measured by the caller, so
/// a loaded download can show whether inflation is queue/pacing or genuine
/// forwarding RTT. Microsecond sums plus call counts; read and reset by the
/// browser instrumentation. Measurement aid, not a shipping metric.
static RETRIEVAL_THROTTLE_WAIT_US: AtomicU64 = AtomicU64::new(0);
static RETRIEVAL_THROTTLE_CALLS: AtomicU64 = AtomicU64::new(0);
static RETRIEVAL_INFLIGHT_CAPPED: AtomicU64 = AtomicU64::new(0);
/// Sum of the *intended* allowance sleeps (the bucket's wait hints), separate
/// from the wall-clock above. Wall minus intended is executor scheduling delay
/// on the saturated single thread, distinguishing true pacing from backlog.
static RETRIEVAL_THROTTLE_SLEEP_US: AtomicU64 = AtomicU64::new(0);
/// Legs that paced at all (the bucket refused at least once).
static RETRIEVAL_THROTTLE_PACED: AtomicU64 = AtomicU64::new(0);
/// Maximum per-peer unsettled debt (AU, counted the remote's way) observed by
/// the debt gate across all admission decisions. The compliance proof: it must
/// stay below the remote's disconnect line for the duration of a sustained
/// download. A measurement aid, not a shipping metric.
static RETRIEVAL_MAX_PEER_DEBT: AtomicU64 = AtomicU64::new(0);
/// Count of admission decisions the hard debt gate refused (debt + price would
/// cross the payment threshold). A non-zero value means the gate is actively
/// bounding per-peer debt; pairs with the unchanged io-reset count to show the
/// gate, not a disconnect, is what relieves the pressure.
static RETRIEVAL_DEBT_GATED: AtomicU64 = AtomicU64::new(0);

/// Record the largest per-peer debt the gate has seen, keeping the running max.
fn observe_peer_debt(debt: u64) {
    let mut current = RETRIEVAL_MAX_PEER_DEBT.load(Ordering::Relaxed);
    while debt > current {
        match RETRIEVAL_MAX_PEER_DEBT.compare_exchange_weak(
            current,
            debt,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}

/// Snapshot the debt-gate proof counters: `(max_peer_debt_au, debt_gated)`.
/// Cumulative; `max_peer_debt_au` is the running maximum.
pub fn retrieval_debt_stats() -> (u64, u64) {
    (
        RETRIEVAL_MAX_PEER_DEBT.load(Ordering::Relaxed),
        RETRIEVAL_DEBT_GATED.load(Ordering::Relaxed),
    )
}

/// Snapshot the retrieval throttle-wait decomposition: `(total_wall_us, calls,
/// inflight_capped, total_intended_sleep_us, paced_legs)`. Cumulative.
pub fn retrieval_throttle_stats() -> (u64, u64, u64, u64, u64) {
    (
        RETRIEVAL_THROTTLE_WAIT_US.load(Ordering::Relaxed),
        RETRIEVAL_THROTTLE_CALLS.load(Ordering::Relaxed),
        RETRIEVAL_INFLIGHT_CAPPED.load(Ordering::Relaxed),
        RETRIEVAL_THROTTLE_SLEEP_US.load(Ordering::Relaxed),
        RETRIEVAL_THROTTLE_PACED.load(Ordering::Relaxed),
    )
}

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

    /// The `_total` counter name incremented when a request of this kind had to
    /// wait for one of the peer's in-flight slots to free up.
    fn inflight_capped_metric(self) -> &'static str {
        match self {
            ProtocolKind::Pushsync => "pushsync_inflight_capped_total",
            ProtocolKind::Retrieval => "retrieval_inflight_capped_total",
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

/// Maximum requests we keep in flight to a single peer at once.
///
/// Mirrors the reference client's small per-peer multiplex fan-out: a serving
/// peer caps the inbound streams it will accept from us before it resets them,
/// so holding only a few requests outstanding to any one peer keeps us under
/// that budget. The cap is per-peer, so a wide retrieval race still runs at full
/// aggregate width; it only bounds the depth piled on each peer. A request to a
/// full peer is skipped to the next-closest peer rather than queued, so a wide
/// download fan-out spreads across the neighbourhood instead of concentrating on
/// the closest few and resetting their streams.
const MAX_INFLIGHT_PER_PEER: usize = 8;

/// Live per-peer in-flight cap, defaulting to [`MAX_INFLIGHT_PER_PEER`].
///
/// A wide concurrent download spreads its fan-out across the neighbourhood, but
/// the deep-leaf region of a large file is served by a small set of close peers,
/// so those few peers' slots are the throughput bound. This override lets the
/// operating point be tuned to the remote's actual multiplex tolerance without a
/// rebuild; the compiled default stays conservative.
static INFLIGHT_PER_PEER: AtomicU64 = AtomicU64::new(MAX_INFLIGHT_PER_PEER as u64);

/// Set the per-peer in-flight retrieval/pushsync cap. A zero is ignored.
pub fn set_inflight_per_peer(cap: usize) {
    if cap > 0 {
        INFLIGHT_PER_PEER.store(cap as u64, Ordering::Relaxed);
    }
}

fn inflight_per_peer() -> usize {
    INFLIGHT_PER_PEER.load(Ordering::Relaxed) as usize
}

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
    /// Per-peer concurrency permits, each a [`MAX_INFLIGHT_PER_PEER`]-permit
    /// semaphore created on first use. A request holds one permit for its
    /// lifetime, so at most that many requests are outstanding to one peer.
    inflight: Mutex<HashMap<OverlayAddress, Arc<Semaphore>>>,
    /// Awaitable debtor-initiated settle, the pre-pay seam. When a request would
    /// be admitted to a peer already past the early-payment trigger
    /// ([`PeerAffordability::should_settle`]), the throttle settles that peer
    /// first so the request is pre-paid and the committed debt never crosses the
    /// line the remote drops us at. Absent (`None`) on a throttle built without
    /// settlement (the in-memory test helper), where requests only pace.
    settle: Option<Arc<dyn PeerSettle>>,
    /// Peers with a background settle already spawned and not yet resolved.
    ///
    /// The non-blocking path spawns a settle for every gated request, but the
    /// provider rate-limits to one offer per peer per second, so a wide download
    /// that gates a peer hundreds of times per second would otherwise flood the
    /// single-thread executor with redundant settle futures that compete with the
    /// retrieval poll and starve each other (the settlement freeze under sustained
    /// load). This set collapses that to one in-flight settle per peer: a spawn is
    /// skipped while the peer is present, and the spawned future removes the peer
    /// on completion so the next gate re-drives it. Shared `Arc` so the spawned
    /// future can clear its own entry.
    settling: Arc<Mutex<std::collections::HashSet<OverlayAddress>>>,
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
            inflight: Mutex::new(HashMap::new()),
            settle: None,
            settling: Arc::new(Mutex::new(std::collections::HashSet::new())),
        }
    }

    /// Spawn a background settle for `peer`, deduplicated so at most one is in
    /// flight per peer at a time.
    ///
    /// The non-blocking admission path calls this whenever it gates or pre-pays a
    /// peer, which under a sustained download is hundreds of times per second per
    /// peer. The provider only acts on one offer per peer per second, so spawning
    /// one future per call would flood the single-thread executor and starve the
    /// settles it is trying to run. The dedup set admits one spawn per peer until
    /// it resolves, then the future clears the entry so the next gate re-drives
    /// it: settlement keeps pace instead of drowning in redundant futures.
    fn spawn_settle(&self, peer: OverlayAddress) {
        let Some(settle) = &self.settle else {
            return;
        };
        let Ok(executor) = TaskExecutor::try_current() else {
            return;
        };
        if !self.settling.lock().insert(peer) {
            // A settle for this peer is already in flight.
            return;
        }
        let fut = settle.settle(peer);
        let settling = Arc::clone(&self.settling);
        executor.spawn(Box::pin(async move {
            fut.await;
            settling.lock().remove(&peer);
        }));
    }

    /// Attach the awaitable debtor-initiated settle so a request to a peer past
    /// the early-payment trigger pre-pays before dispatch.
    ///
    /// Without it the throttle only paces; with it a blocking acquire settles and
    /// waits before admitting, and a non-blocking try-acquire kicks off a settle
    /// and skips the peer for this poll so a later poll finds it affordable. The
    /// settle source is the same shared accounting the pacing reads, so the
    /// pre-pay and the allowance view never diverge.
    #[must_use]
    pub(crate) fn with_settle(mut self, settle: Arc<dyn PeerSettle>) -> Self {
        self.settle = Some(settle);
        self
    }

    /// The peer's concurrency semaphore, created on first use.
    fn peer_semaphore(&self, peer: &OverlayAddress) -> Arc<Semaphore> {
        Arc::clone(
            self.inflight
                .lock()
                .entry(*peer)
                .or_insert_with(|| Arc::new(Semaphore::new(inflight_per_peer()))),
        )
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

    /// True if admitting `address` to `peer` would push our unsettled debt
    /// counted the remote's way (committed plus in-flight reservation, plus this
    /// request's price) past the peer's payment threshold.
    ///
    /// The payment threshold sits below the remote's disconnect line by the
    /// payment-tolerance margin, so a gate that refuses admission here keeps the
    /// debt the remote sees strictly under the line it drops us at. The committed
    /// debit lands on delivery and the reservation is taken at dispatch, so this
    /// matches the remote's `PeerDebt = balance + shadowReserved` view ahead of
    /// our own delivery debit. A zero payment threshold (the in-memory test
    /// helper, which tracks no accounting) never gates.
    fn debt_would_exceed_threshold(&self, peer: &OverlayAddress, address: &ChunkAddress) -> bool {
        let ceiling = self.allowance.payment_threshold(peer);
        if ceiling == Au::ZERO {
            return false;
        }
        let debt = self.allowance.unsettled_debt(peer);
        observe_peer_debt(debt.as_amount());
        let price = self.pricing.peer_price(peer, address);
        debt.saturating_add(price) > ceiling
    }

    /// Admit a request for `address` to `peer`, returning a permit the caller
    /// holds for the request's lifetime.
    ///
    /// First waits for a per-peer concurrency permit
    /// ([`MAX_INFLIGHT_PER_PEER`]): the returned [`ThrottlePermit`] holds it, so
    /// while it is alive one of the peer's in-flight slots is taken and a later
    /// request to the same peer waits here once the slots are full. Then paces
    /// against the peer's allowance bucket: returns once the bucket has room,
    /// otherwise sleeping the bucket's wait hint and retrying, re-syncing the live
    /// allowance each iteration so a growing allowance shortens the wait and a
    /// shrinking one lengthens it. The first delay increments the per-peer
    /// throttle metric for `kind`.
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
    ) -> Option<ThrottlePermit> {
        // Reserve one of the peer's in-flight slots without blocking. A full peer
        // returns `None`: the caller skips it and races the next-closest peer
        // instead of serialising behind this one's slots. The permit is held for
        // the request's lifetime, freeing the slot on drop. The semaphore is never
        // closed, so the only error is a closed semaphore; treat it as no slot.
        let slot = match self.peer_semaphore(&peer).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                if kind == ProtocolKind::Retrieval {
                    RETRIEVAL_INFLIGHT_CAPPED.fetch_add(1, Ordering::Relaxed);
                }
                metrics::counter!(
                    kind.inflight_capped_metric(),
                    "peer_overlay" => peer.to_string(),
                )
                .increment(1);
                return None;
            }
        };

        // Record how long this retrieval leg paces inside the throttle (allowance
        // wait), separately from the on-wire RTT the caller times, so a loaded
        // download can attribute latency inflation to pacing vs forwarding.
        let throttle_started = (kind == ProtocolKind::Retrieval).then(Instant::now);
        let record_wait = |started: Option<Instant>| {
            if let Some(start) = started {
                RETRIEVAL_THROTTLE_WAIT_US
                    .fetch_add(start.elapsed().as_micros() as u64, Ordering::Relaxed);
                RETRIEVAL_THROTTLE_CALLS.fetch_add(1, Ordering::Relaxed);
            }
        };

        // Hard debt gate, blocking variant: if admitting would push our unsettled
        // debt past the peer's payment threshold, settle-and-wait first so the
        // debt drains below the ceiling before the request goes out. The blocking
        // path is the candidate-walk fallback, so settling synchronously here
        // (rather than skipping) is what guarantees progress when every candidate
        // is momentarily over the ceiling: the settle drains the debt and the
        // re-check below admits. If settlement cannot drain it (no settle attached
        // or the creditor refuses), the request still goes out after the wait
        // rather than hanging; the remote may refuse it, which is recoverable.
        if self.debt_would_exceed_threshold(&peer, &address) {
            if kind == ProtocolKind::Retrieval {
                RETRIEVAL_DEBT_GATED.fetch_add(1, Ordering::Relaxed);
            }
            if let Some(settle) = &self.settle {
                settle.settle(peer).await;
            }
        } else if let Some(settle) = &self.settle
            && self.allowance.should_settle(&peer)
        {
            // Below the hard ceiling but past the softer early-payment trigger:
            // pre-pay so the committed debit on delivery does not carry across to
            // the next request. The provider rate-limits per peer, so a settle
            // already in flight resolves cheaply without a redundant offer.
            settle.settle(peer).await;
        }

        let cost = self.request_cost(&peer, &address);
        let mut throttled = false;
        let mut waited = Duration::ZERO;
        for _ in 0..MAX_THROTTLE_ITERATIONS {
            self.sync_quota(&peer);
            let decision = self.limiter.lock().try_send(peer, cost);
            match decision {
                Ok(()) => {
                    record_wait(throttle_started);
                    return Some(ThrottlePermit { _slot: slot });
                }
                Err(delay) => {
                    if !throttled {
                        throttled = true;
                        if throttle_started.is_some() {
                            RETRIEVAL_THROTTLE_PACED.fetch_add(1, Ordering::Relaxed);
                        }
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
                    if throttle_started.is_some() {
                        RETRIEVAL_THROTTLE_SLEEP_US
                            .fetch_add(wait.as_micros() as u64, Ordering::Relaxed);
                    }
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
        record_wait(throttle_started);
        Some(ThrottlePermit { _slot: slot })
    }

    /// Non-blocking admission: return a permit only if the peer admits the
    /// request *right now*, else `None`.
    ///
    /// The distributed retrieval scheduler polls this across the connected set
    /// in proximity order and assigns each chunk to the first peer that admits,
    /// so a busy or over-allowance peer is skipped to the next-closest rather
    /// than waited on. Admission needs both a free in-flight slot *and* live
    /// allowance headroom for the chunk's exact price: unlike [`Self::acquire`],
    /// it never sleeps the bucket's wait hint, so a peer whose bucket is
    /// momentarily empty returns `None` immediately instead of pacing.
    pub(crate) fn try_acquire(
        &self,
        peer: OverlayAddress,
        address: ChunkAddress,
        kind: ProtocolKind,
    ) -> Option<ThrottlePermit> {
        // Hard debt gate: if admitting this request would push our unsettled debt
        // counted the remote's way past its payment threshold, skip the peer. The
        // distributed scheduler then assigns the chunk to the next-closest peer,
        // and a background settle drains this peer's debt so a later poll readmits
        // it below the line. This is the load-bearing brake: under a sustained
        // download the per-peer debt is what the remote resets us on, so refusing
        // admission before the debt crosses the line is what keeps us connected,
        // independently of how the rate bucket happens to be sized. The gate runs
        // before the slot reservation so a gated request holds nothing.
        if self.debt_would_exceed_threshold(&peer, &address) {
            self.spawn_settle(peer);
            if kind == ProtocolKind::Retrieval {
                RETRIEVAL_THROTTLE_PACED.fetch_add(1, Ordering::Relaxed);
                RETRIEVAL_DEBT_GATED.fetch_add(1, Ordering::Relaxed);
            }
            metrics::counter!(
                kind.throttled_metric(),
                "peer_overlay" => peer.to_string(),
            )
            .increment(1);
            return None;
        }

        // Pre-pay, non-blocking variant: a peer past the early-payment trigger but
        // still under the debt ceiling has a settle kicked off in the background
        // so its debt drains while the scheduler keeps admitting through the
        // bucket. Skipping it on the softer `should_settle` trigger alone would
        // shift load to farther peers and re-dial, churning the neighbourhood; the
        // hard gate above is what bounds the debt. Deduplicated so a wide download
        // does not flood the executor with redundant settles for the same peer.
        if self.settle.is_some() && self.allowance.should_settle(&peer) {
            self.spawn_settle(peer);
        }

        let slot = match self.peer_semaphore(&peer).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                if kind == ProtocolKind::Retrieval {
                    RETRIEVAL_INFLIGHT_CAPPED.fetch_add(1, Ordering::Relaxed);
                }
                metrics::counter!(
                    kind.inflight_capped_metric(),
                    "peer_overlay" => peer.to_string(),
                )
                .increment(1);
                return None;
            }
        };

        let cost = self.request_cost(&peer, &address);
        self.sync_quota(&peer);
        match self.limiter.lock().try_send(peer, cost) {
            Ok(()) => Some(ThrottlePermit { _slot: slot }),
            Err(_) => {
                if kind == ProtocolKind::Retrieval {
                    RETRIEVAL_THROTTLE_PACED.fetch_add(1, Ordering::Relaxed);
                }
                metrics::counter!(
                    kind.throttled_metric(),
                    "peer_overlay" => peer.to_string(),
                )
                .increment(1);
                // Drop the slot permit so a peer denied on allowance does not
                // hold an in-flight slot it never used.
                None
            }
        }
    }

    /// Drop the peer's bucket on disconnect so memory does not grow with the
    /// count of distinct peers seen, and a later reconnect starts from a fresh
    /// allowance rather than stale credit.
    pub fn clear(&self, peer: &OverlayAddress) {
        self.limiter.lock().clear(peer);
        // Drop the peer's semaphore so its entry does not outlive the peer. Any
        // permits still outstanding keep their own `Arc` alive until their
        // requests finish; a reconnect creates a fresh full semaphore.
        self.inflight.lock().remove(peer);
    }
}

/// Admission permit for one outbound request.
///
/// Holds the peer's in-flight slot for the request's lifetime: keep it alive
/// until the request completes, then drop it to free the slot for the next
/// request to that peer. Carries no data; its only effect is the held permit.
#[must_use = "dropping the permit immediately frees the peer's in-flight slot"]
pub(crate) struct ThrottlePermit {
    _slot: OwnedSemaphorePermit,
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

    /// Allowance whose `should_settle` verdict is swappable, so the pre-pay gate
    /// can be exercised independently of the bucket arithmetic. Always affordable
    /// so only the settle gate, not the rate limiter, governs admission.
    struct SettleSignal {
        settle: std::sync::atomic::AtomicBool,
    }

    impl SettleSignal {
        fn new(should_settle: bool) -> Arc<Self> {
            Arc::new(Self {
                settle: std::sync::atomic::AtomicBool::new(should_settle),
            })
        }
        fn set(&self, should_settle: bool) {
            self.settle.store(should_settle, Ordering::SeqCst);
        }
    }

    impl PeerAffordability for SettleSignal {
        fn can_afford(&self, _overlay: &OverlayAddress, _price: Au) -> bool {
            true
        }
        fn allowance_remaining(&self, _overlay: &OverlayAddress) -> Au {
            Au::from_amount(u64::from(u32::MAX))
        }
        fn should_settle(&self, _overlay: &OverlayAddress) -> bool {
            self.settle.load(Ordering::SeqCst)
        }
    }

    /// Records every peer the pre-pay gate asks to settle. The future resolves
    /// immediately; the test asserts on the recorded calls.
    #[derive(Default)]
    struct RecordingSettle {
        settled: Mutex<Vec<OverlayAddress>>,
    }

    impl PeerSettle for RecordingSettle {
        fn settle(&self, peer: OverlayAddress) -> futures::future::BoxFuture<'static, ()> {
            self.settled.lock().push(peer);
            Box::pin(async {})
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
        fn should_settle(&self, overlay: &OverlayAddress) -> bool {
            self.0.should_settle(overlay)
        }
        fn unsettled_debt(&self, overlay: &OverlayAddress) -> Au {
            self.0.unsettled_debt(overlay)
        }
        fn payment_threshold(&self, overlay: &OverlayAddress) -> Au {
            self.0.payment_threshold(overlay)
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
        let _ = tokio::time::timeout(
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
        // Drop the first permit immediately so only the rate bucket, not the
        // in-flight cap, gates the second acquire.
        drop(t.acquire(peer(1), address(1), ProtocolKind::Pushsync).await);
        let _ = tokio::time::timeout(
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
        let _ = tokio::time::timeout(
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

    #[tokio::test]
    async fn inflight_cap_skips_beyond_the_limit_then_admits_after_release() {
        // A generous allowance never rate-throttles, so the per-peer in-flight
        // cap is the only bound: the first MAX_INFLIGHT_PER_PEER acquires hold
        // their slots, the next is skipped (`None`) rather than blocking, and a
        // freed slot admits the next request.
        let alloc = DynamicAllowance::new(u64::from(u32::MAX));
        let t = throttle(alloc.clone());

        let mut permits = Vec::new();
        for _ in 0..MAX_INFLIGHT_PER_PEER {
            permits.push(
                t.acquire(peer(1), address(1), ProtocolKind::Retrieval)
                    .await
                    .expect("slots up to the cap are admitted at once"),
            );
        }

        // The next acquire is skipped without blocking: no slot is free.
        assert!(
            t.acquire(peer(1), address(1), ProtocolKind::Retrieval)
                .await
                .is_none(),
            "an acquire beyond the in-flight cap is skipped, not blocked"
        );

        // Drop one permit; an acquire now succeeds.
        permits.pop();
        assert!(
            t.acquire(peer(1), address(1), ProtocolKind::Retrieval)
                .await
                .is_some(),
            "a freed slot admits the next request"
        );
    }

    #[tokio::test]
    async fn try_acquire_admits_under_budget_and_skips_when_empty() {
        // A one-request bucket admits the first non-blocking acquire, then refuses
        // the second immediately (no wait): the distributed scheduler relies on
        // this skip-don't-wait behaviour to fall through to the next-closest peer.
        let alloc = DynamicAllowance::new(COST);
        let t = throttle(alloc.clone());
        let first = t.try_acquire(peer(1), address(1), ProtocolKind::Retrieval);
        assert!(first.is_some(), "first non-blocking acquire admits");
        assert!(
            t.try_acquire(peer(1), address(1), ProtocolKind::Retrieval)
                .is_none(),
            "an empty bucket is skipped, not waited on"
        );
        // A different peer with its own bucket still admits.
        assert!(
            t.try_acquire(peer(2), address(1), ProtocolKind::Retrieval)
                .is_some(),
            "a distinct peer's bucket is independent"
        );
    }

    #[tokio::test]
    async fn try_acquire_skips_a_full_peer_without_holding_a_slot() {
        // A generous allowance never rate-throttles, so the in-flight cap is the
        // only bound: the first MAX_INFLIGHT_PER_PEER non-blocking acquires hold
        // their slots and the next is skipped without blocking.
        let alloc = DynamicAllowance::new(u64::from(u32::MAX));
        let t = throttle(alloc.clone());
        let mut permits = Vec::new();
        for _ in 0..MAX_INFLIGHT_PER_PEER {
            permits.push(
                t.try_acquire(peer(1), address(1), ProtocolKind::Retrieval)
                    .expect("slots up to the cap admit at once"),
            );
        }
        assert!(
            t.try_acquire(peer(1), address(1), ProtocolKind::Retrieval)
                .is_none(),
            "a full peer is skipped, not blocked"
        );
        permits.pop();
        assert!(
            t.try_acquire(peer(1), address(1), ProtocolKind::Retrieval)
                .is_some(),
            "a freed slot admits the next request"
        );
    }

    #[tokio::test]
    async fn inflight_cap_is_per_peer() {
        // Saturating one peer's slots must not affect a different peer: the
        // retrieval race across many peers stays full-width.
        let alloc = DynamicAllowance::new(u64::from(u32::MAX));
        let t = throttle(alloc.clone());

        let mut held = Vec::new();
        for _ in 0..MAX_INFLIGHT_PER_PEER {
            held.push(
                t.acquire(peer(1), address(1), ProtocolKind::Retrieval)
                    .await
                    .expect("peer(1) slots fill"),
            );
        }

        // peer(2) has its own full set of slots.
        assert!(
            t.acquire(peer(2), address(1), ProtocolKind::Retrieval)
                .await
                .is_some(),
            "a different peer's slots are independent"
        );
    }

    #[tokio::test]
    async fn acquire_pre_pays_a_peer_at_the_settlement_trigger() {
        // A blocking acquire to a peer past the early-payment trigger settles
        // that peer before admitting, so the request that follows is pre-paid and
        // the committed debit cannot push the peer over the disconnect line.
        let signal = SettleSignal::new(true);
        let recorder = Arc::new(RecordingSettle::default());
        let t = build_throttle(
            signal.clone(),
            Arc::new(FixedPrice(COST)),
            REFRESH_RATE,
            100,
        )
        .with_settle(recorder.clone());

        let permit = t
            .acquire(peer(1), address(1), ProtocolKind::Retrieval)
            .await;
        assert!(
            permit.is_some(),
            "an affordable peer still admits after settling"
        );
        assert_eq!(
            recorder.settled.lock().as_slice(),
            &[peer(1)],
            "the peer at the trigger is settled before the request goes out"
        );

        // Below the trigger, no pre-pay settle fires.
        signal.set(false);
        let _ = t
            .acquire(peer(2), address(1), ProtocolKind::Retrieval)
            .await;
        assert_eq!(
            recorder.settled.lock().len(),
            1,
            "a peer below the trigger is not settled"
        );
    }

    #[tokio::test]
    async fn try_acquire_admits_a_peer_at_the_trigger_while_settling() {
        // The non-blocking path does NOT skip a peer past the trigger: skipping
        // the (typically closest) peer would shift load to farther peers and
        // re-dial. It admits through the bucket and drains the debt with a
        // background settle (kicked off only when an executor is present, not in
        // this unit test). The admission itself is what this asserts: a generous
        // allowance plus a peer at the trigger must still admit.
        let signal = SettleSignal::new(true);
        let recorder = Arc::new(RecordingSettle::default());
        let t = build_throttle(
            signal.clone(),
            Arc::new(FixedPrice(COST)),
            REFRESH_RATE,
            100,
        )
        .with_settle(recorder.clone());

        assert!(
            t.try_acquire(peer(1), address(1), ProtocolKind::Retrieval)
                .is_some(),
            "a peer at the trigger still admits through the bucket"
        );
    }

    /// Affordability whose unsettled debt and payment threshold are settable, so
    /// the hard debt gate can be exercised independently of the rate bucket.
    /// Always rate-affordable (a wide allowance) so only the debt gate governs.
    struct DebtSignal {
        debt: AtomicU64,
        threshold: AtomicU64,
    }

    impl DebtSignal {
        fn new(debt: u64, threshold: u64) -> Arc<Self> {
            Arc::new(Self {
                debt: AtomicU64::new(debt),
                threshold: AtomicU64::new(threshold),
            })
        }
        fn set_debt(&self, debt: u64) {
            self.debt.store(debt, Ordering::SeqCst);
        }
    }

    impl PeerAffordability for DebtSignal {
        fn can_afford(&self, _overlay: &OverlayAddress, _price: Au) -> bool {
            true
        }
        fn allowance_remaining(&self, _overlay: &OverlayAddress) -> Au {
            Au::from_amount(u64::from(u32::MAX))
        }
        fn allowance_to_payment_threshold(&self, _overlay: &OverlayAddress) -> Au {
            Au::from_amount(u64::from(u32::MAX))
        }
        fn unsettled_debt(&self, _overlay: &OverlayAddress) -> Au {
            Au::from_amount(self.debt.load(Ordering::SeqCst))
        }
        fn payment_threshold(&self, _overlay: &OverlayAddress) -> Au {
            Au::from_amount(self.threshold.load(Ordering::SeqCst))
        }
    }

    #[tokio::test]
    async fn try_acquire_skips_a_peer_over_the_debt_ceiling() {
        // Debt just under the ceiling admits; once debt + price would cross the
        // payment threshold, the non-blocking path skips the peer (returns None)
        // so the scheduler assigns the chunk elsewhere. This is the core gate: it
        // bounds per-peer debt below the line the remote drops us at, regardless
        // of the rate bucket. Threshold 100, price COST (10): debt 80 leaves room
        // (90 <= 100) but debt 95 would cross (105 > 100).
        let signal = DebtSignal::new(80, 100);
        let recorder = Arc::new(RecordingSettle::default());
        let t = build_throttle(
            signal.clone(),
            Arc::new(FixedPrice(COST)),
            REFRESH_RATE,
            100,
        )
        .with_settle(recorder.clone());

        assert!(
            t.try_acquire(peer(1), address(1), ProtocolKind::Retrieval)
                .is_some(),
            "a peer under the debt ceiling admits"
        );

        signal.set_debt(95);
        assert!(
            t.try_acquire(peer(1), address(1), ProtocolKind::Retrieval)
                .is_none(),
            "a peer whose debt plus this request would cross the payment threshold is skipped"
        );
    }

    #[tokio::test]
    async fn try_acquire_does_not_gate_when_threshold_is_zero() {
        // A zero payment threshold (no accounting, e.g. the in-memory helper) must
        // never gate: the debt gate is a no-op so the bucket alone governs.
        let signal = DebtSignal::new(u64::from(u32::MAX), 0);
        let t = throttle_with(signal);
        assert!(
            t.try_acquire(peer(1), address(1), ProtocolKind::Retrieval)
                .is_some(),
            "a zero threshold disables the debt gate"
        );
    }

    #[tokio::test]
    async fn acquire_settles_then_admits_a_peer_over_the_debt_ceiling() {
        // The blocking path settles-and-waits when over the ceiling (rather than
        // skipping), guaranteeing progress on the candidate-walk fallback. With a
        // recording settle that does not actually drain the debt, the request is
        // still released after the settle so the future never hangs.
        let signal = DebtSignal::new(u64::from(u32::MAX), 100);
        let recorder = Arc::new(RecordingSettle::default());
        let t = build_throttle(signal, Arc::new(FixedPrice(COST)), REFRESH_RATE, 100)
            .with_settle(recorder.clone());

        let permit = tokio::time::timeout(
            Duration::from_secs(2),
            t.acquire(peer(1), address(1), ProtocolKind::Retrieval),
        )
        .await
        .expect("over-ceiling acquire releases after settling, never hangs");
        assert!(permit.is_some(), "the request is released after the settle");
        assert_eq!(
            recorder.settled.lock().as_slice(),
            &[peer(1)],
            "the over-ceiling peer is settled before the request goes out"
        );
    }

    fn throttle_with(allowance: Arc<dyn PeerAffordability>) -> SelfThrottle {
        build_throttle(allowance, Arc::new(FixedPrice(COST)), REFRESH_RATE, 100)
    }

    /// A settle that counts how many times it was invoked, so the dedup test can
    /// assert that repeated gating of one peer does not spawn a settle each time.
    #[derive(Default)]
    struct CountingSettle {
        calls: AtomicU64,
    }

    impl CountingSettle {
        fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }
    }

    impl PeerSettle for CountingSettle {
        fn settle(&self, _peer: OverlayAddress) -> futures::future::BoxFuture<'static, ()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {})
        }
    }

    #[tokio::test]
    async fn gated_settles_are_deduplicated_per_peer() {
        // Repeatedly gating the same peer must spawn at most one settle while one
        // is in flight: the dedup set bounds the executor load so settlement is
        // not drowned by redundant futures under a sustained download. The test
        // installs a task manager so `TaskExecutor::try_current` resolves, then
        // pre-seeds the dedup set to model an in-flight settle and asserts a
        // second gate does not re-spawn.
        let _manager = vertex_tasks::TaskManager::current();
        let signal = DebtSignal::new(u64::from(u32::MAX), 100); // always over the ceiling
        let recorder = CountingSettle::new();
        let t = build_throttle(signal, Arc::new(FixedPrice(COST)), REFRESH_RATE, 100)
            .with_settle(recorder.clone());

        // First gate spawns one settle and marks the peer as settling.
        assert!(
            t.try_acquire(peer(1), address(1), ProtocolKind::Retrieval)
                .is_none()
        );
        assert!(
            t.settling.lock().contains(&peer(1)),
            "the gated peer is recorded as settling"
        );

        // While that settle is recorded in flight, further gates of the same peer
        // do not spawn another: the dedup set already holds it.
        let before = recorder.calls.load(Ordering::SeqCst);
        for _ in 0..50 {
            let _ = t.try_acquire(peer(1), address(1), ProtocolKind::Retrieval);
        }
        // The immediate-resolving recorder may have cleared and re-armed once or
        // twice as the executor drains; the invariant is that 50 gates did not
        // produce ~50 spawns. A small constant proves the flood is collapsed.
        let after = recorder.calls.load(Ordering::SeqCst);
        assert!(
            after - before <= 5,
            "50 gates produced {} settles; dedup did not collapse the flood",
            after - before
        );
    }
}
