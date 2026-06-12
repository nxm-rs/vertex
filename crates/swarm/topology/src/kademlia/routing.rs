//! Kademlia-based peer routing for Swarm.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
    time::Duration,
};

use super::{
    CandidateSelector, CandidateSnapshot, DepthAwareLimits, KademliaConfig, LimitsSnapshot,
    PhaseTracker, PhaseTransition, RoutingCapacity, SwarmRouting, TopologyPhase,
    candidate_queues::CandidateQueues, select_balanced_candidates, select_neighborhood_candidates,
};
use crate::metrics::{phase, record_phase_transition, record_topology_phase_change};
use nectar_primitives::{ChunkAddress, recompute_neighborhood_depth};
use parking_lot::{Mutex, RwLock};
// The neighborhood stability clock is a `tokio::time::Instant` on native so the
// paused-time tests can advance it deterministically. tokio's clock reaches the
// std monotonic clock, which panics on wasm32-unknown-unknown, so the browser
// build uses the `web-time` clock instead. Both expose `now`, `elapsed`, and the
// duration arithmetic this module uses.
#[cfg(not(target_arch = "wasm32"))]
use tokio::time::Instant;
use tracing::{debug, info, trace};
use vertex_swarm_api::{SwarmIdentity, SwarmSpec};
use vertex_swarm_peer_manager::{PeerManager, ProximityIndex};
use vertex_swarm_primitives::{
    Bin, NeighborhoodDepth, OverlayAddress, ProximityOrder, SwarmNodeType, all_bins, balanced_bins,
    neighborhood_bins,
};
#[cfg(target_arch = "wasm32")]
use vertex_util_runtime::time::Instant;
use vertex_util_runtime::time::Instant as PhaseInstant;

/// Connection phase for capacity tracking.
#[derive(PartialEq, Eq)]
enum ConnectionPhase {
    Dialing,
    Handshaking,
    Active,
}

/// Phase of a connection being considered for eviction.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EvictionPhase {
    Handshaking,
    Active,
}

/// A peer identified for eviction due to bin overpopulation.
pub(crate) struct EvictionCandidate {
    pub(crate) overlay: OverlayAddress,
    pub(crate) bin: Bin,
    pub(crate) phase: EvictionPhase,
}

/// The saturated-neighborhood state the stability clock is anchored to.
///
/// Held while the neighborhood (bins at and above `depth`) collectively
/// holds at least the saturation threshold in connected peers. A depth
/// change replaces the anchor (restarting the clock); a saturation dip
/// clears it.
struct NeighborhoodStable {
    /// The depth at which the neighborhood became saturated.
    depth: NeighborhoodDepth,
    /// When the neighborhood became saturated at `depth`.
    ///
    /// `tokio::time::Instant` rather than `std::time::Instant` so paused-time
    /// tests can drive the stability window deterministically; outside a
    /// tokio runtime it falls back to the real clock.
    since: Instant,
}

fn atomic_inc(vec: &[AtomicUsize], bin: Bin) {
    if let Some(c) = vec.get(bin.as_index()) {
        c.fetch_add(1, Ordering::Relaxed);
    }
}

fn atomic_dec(vec: &[AtomicUsize], bin: Bin) {
    if let Some(c) = vec.get(bin.as_index()) {
        c.fetch_sub(1, Ordering::Relaxed);
    }
}

fn atomic_load(vec: &[AtomicUsize], bin: Bin) -> usize {
    vec.get(bin.as_index())
        .map_or(0, |c| c.load(Ordering::Relaxed))
}

fn make_atomic_vec(n: usize) -> Vec<AtomicUsize> {
    (0..n).map(|_| AtomicUsize::new(0)).collect()
}

/// Select `count` trim victims from one bin's active peers, worst first.
///
/// Each pool entry carries the caller's rank and the peer score. The primary
/// order is unchanged from plain rank-based trimming: the lowest `(rank,
/// score)` pair is evicted soonest. Among peers still tied on both, prefix
/// diversity decides: the victim is the tied peer whose address shares the
/// longest prefix (highest [`ProximityOrder`]) with any other remaining peer,
/// so the retained set stays spread across the bin's sub-tries - the same
/// balance goal candidate selection pursues when filling the bin. A residual
/// tie falls back to overlay order so selection is deterministic.
///
/// Greedy and incremental: one victim per round, with redundancy recomputed
/// against the survivors, O(count * n^2) for a bin of `n` peers. Bins hold at
/// most a few dozen peers, so the quadratic term is negligible.
fn select_trim_victims<R: Ord>(
    mut pool: Vec<(OverlayAddress, R, f64)>,
    count: usize,
) -> Vec<OverlayAddress> {
    use std::cmp::Ordering;

    if count >= pool.len() {
        return pool.into_iter().map(|(overlay, _, _)| overlay).collect();
    }

    /// Worst-first primary order: rank, then score (`NaN`-tolerant).
    fn by_rank_then_score<R: Ord>(
        a: &(OverlayAddress, R, f64),
        b: &(OverlayAddress, R, f64),
    ) -> Ordering {
        a.1.cmp(&b.1)
            .then_with(|| a.2.partial_cmp(&b.2).unwrap_or(Ordering::Equal))
    }

    let mut victims = Vec::with_capacity(count);
    while victims.len() < count {
        // Redundancy of each survivor: the longest prefix it shares with any
        // other survivor. A high value means a same-sub-trie sibling remains,
        // so evicting this peer costs the least diversity.
        let redundancy: Vec<Option<ProximityOrder>> = pool
            .iter()
            .map(|(overlay, _, _)| {
                pool.iter()
                    .filter(|(other, _, _)| other != overlay)
                    .map(|(other, _, _)| overlay.proximity(other))
                    .max()
            })
            .collect();

        let victim = pool
            .iter()
            .zip(&redundancy)
            .enumerate()
            .min_by(|(_, (a, a_redundancy)), (_, (b, b_redundancy))| {
                by_rank_then_score(a, b)
                    // Higher redundancy is evicted first (min_by, so reversed).
                    .then_with(|| b_redundancy.cmp(a_redundancy))
                    .then_with(|| a.0.cmp(&b.0))
            })
            .map(|(idx, _)| idx);

        let Some(idx) = victim else { break };
        victims.push(pool.swap_remove(idx).0);
    }

    victims
}

/// Saturation deficit (in peers, summed across the bins below the published
/// depth) up to which a depth lowering is treated as churn noise and held
/// for the stability window instead of being published immediately.
const DEPTH_LOWER_DEFICIT_TOLERANCE: usize = 1;

/// Kademlia-based peer routing table.
pub(crate) struct KademliaRouting<I: SwarmIdentity> {
    identity: I,
    max_po: u8,
    pub(crate) connected_peers: ProximityIndex,
    peer_manager: Arc<PeerManager<I>>,
    /// Published neighborhood depth. Lowering passes through hysteresis
    /// (see [`Self::publish_depth_at`]); every consumer reads this value
    /// through [`Self::depth`].
    depth: AtomicU8,
    /// Start of the stability window during which a marginal depth lowering
    /// is held back. `Some` while the instantaneous depth sits below the
    /// published depth with a saturation deficit within
    /// [`DEPTH_LOWER_DEFICIT_TOLERANCE`].
    pending_depth_lower: Mutex<Option<Instant>>,
    config: KademliaConfig,
    candidate_queues: CandidateQueues,
    dialing_counts: Vec<AtomicUsize>,
    handshaking_counts: Vec<AtomicUsize>,
    active_counts: Vec<AtomicUsize>,
    connection_phases: RwLock<HashMap<OverlayAddress, ConnectionPhase>>,
    /// Stability clock for the saturated neighborhood; `None` while the
    /// neighborhood is below saturation. Updated on every routing-table
    /// mutation so dips between snapshots are never missed.
    neighborhood_stability: Mutex<Option<NeighborhoodStable>>,
    topology_phase: Mutex<PhaseTracker>,
}

impl<I: SwarmIdentity> KademliaRouting<I> {
    pub(crate) fn new(
        identity: I,
        config: KademliaConfig,
        peer_manager: Arc<PeerManager<I>>,
    ) -> Arc<Self> {
        let max_po = identity.spec().max_po();
        let local_overlay = identity.overlay_address();
        let num_bins = (max_po as usize) + 1;
        // A single bin must be able to hold a full evaluation round's worth
        // of candidates for its category, or part of the round is silently
        // dropped while it waits for the rate-shaped drain.
        let queue_cap = config
            .max_neighbor_candidates
            .max(config.max_balanced_candidates);

        let topology_phase = Mutex::new(PhaseTracker::new(
            config.phase_stability_window,
            PhaseInstant::now(),
        ));
        // Publish the initial phase gauge so operators see Bootstrap from
        // startup rather than no phase until the first transition.
        crate::metrics::set_topology_phase(TopologyPhase::Bootstrap);

        Arc::new(Self {
            identity,
            max_po,
            // connected_peers is unbounded (controlled by routing capacity)
            connected_peers: ProximityIndex::new(local_overlay, max_po, 0),
            peer_manager,
            depth: AtomicU8::new(0),
            pending_depth_lower: Mutex::new(None),
            config,
            candidate_queues: CandidateQueues::new(num_bins, queue_cap),
            dialing_counts: make_atomic_vec(num_bins),
            handshaking_counts: make_atomic_vec(num_bins),
            active_counts: make_atomic_vec(num_bins),
            connection_phases: RwLock::new(HashMap::new()),
            neighborhood_stability: Mutex::new(None),
            topology_phase,
        })
    }

    /// Depth-aware per-bin capacity limits.
    pub(crate) fn limits(&self) -> &DepthAwareLimits {
        &self.config.limits
    }

    /// The resolved routing configuration (build-time pacing assertions).
    #[cfg(test)]
    pub(crate) fn config(&self) -> &KademliaConfig {
        &self.config
    }

    /// Admission decision for a peer whose handshake is currently in
    /// progress.
    ///
    /// `extra` is the number of slots the in-flight peer occupies in the
    /// caller's accounting but is *not* yet counted in
    /// [`Self::effective_count`]:
    ///
    /// * outbound: `0`. The dial planner reserved a `Dialing` slot via
    ///   [`RoutingCapacity::try_reserve_dial`] (transitioned to
    ///   `Handshaking` by [`RoutingCapacity::dial_connected`]) before
    ///   the handshake started, so `effective_count` already includes
    ///   the in-flight peer.
    /// * inbound: `1`. No slot is reserved until topology processes the
    ///   `HandshakeEvent::Completed`, which happens *after* the
    ///   admission gate runs. The check must add one to model the slot
    ///   that will be reserved on success.
    ///
    /// Returns `true` when the bin would still be within `ceiling`
    /// (target plus headroom) after counting the in-flight peer. Bins
    /// inside the neighborhood (where `ceiling == usize::MAX`) always
    /// return `true`; eviction in oversaturated neighborhoods is a
    /// separate concern handled by a future `AcceptEvict` decision.
    pub(crate) fn admission_within_capacity(&self, overlay: &OverlayAddress, extra: usize) -> bool {
        let bin = self.bin_for(overlay);
        let depth = self.depth();
        let ceiling = self.config.limits.ceiling(bin, depth);
        if ceiling == usize::MAX {
            return true;
        }
        self.effective_count(bin).saturating_add(extra) <= ceiling
    }

    fn effective_count(&self, bin: Bin) -> usize {
        atomic_load(&self.dialing_counts, bin)
            + atomic_load(&self.handshaking_counts, bin)
            + atomic_load(&self.active_counts, bin)
    }

    /// Peers in a bin that eviction can actually act on (handshaking and
    /// active). In-flight dials hold capacity but cannot be evicted; counting
    /// them into the trim surplus would force active evictions to pay for
    /// slots the evictable population does not own, cutting the bin below
    /// saturation and flapping depth.
    fn evictable_count(&self, bin: Bin) -> usize {
        atomic_load(&self.handshaking_counts, bin) + atomic_load(&self.active_counts, bin)
    }

    /// The deepest bin in this routing table, as a typed [`Bin`].
    pub(crate) fn max_bin(&self) -> Bin {
        Bin::new(self.max_po).unwrap_or(Bin::MAX)
    }

    fn base(&self) -> OverlayAddress {
        self.identity.overlay_address()
    }

    /// The [`Bin`] a peer occupies in this node's table (its proximity
    /// order to the local overlay, capped at `max_po`).
    fn bin_for(&self, peer: &OverlayAddress) -> Bin {
        Bin::new(self.base().proximity(peer).get().min(self.max_po)).unwrap_or(Bin::MAX)
    }

    /// Capture state for candidate selection (lightweight: banned/backoff checked live).
    #[tracing::instrument(skip(self), level = "trace")]
    fn capture_candidate_state(&self, effective_depth: NeighborhoodDepth) -> CandidateSnapshot {
        let queued_set = self.candidate_queues.snapshot_queued();
        let in_progress: HashSet<OverlayAddress> = vertex_observability::timed_read(
            &self.connection_phases,
            metrics::histogram!("topology_routing_phases_lock_seconds"),
        )
        .keys()
        .copied()
        .collect();

        CandidateSnapshot {
            limits: LimitsSnapshot::capture(&self.config.limits, effective_depth),
            in_progress,
            queued: queued_set,
        }
    }

    /// Recompute neighborhood depth from the given connected-peer bin sizes.
    ///
    /// Delegates to [`recompute_neighborhood_depth`], the canonical port that
    /// walks shallow to deep to find the unsaturated frontier and then anchors
    /// the neighborhood by the low watermark. Crucially this caps depth at the
    /// shallowest empty or unsaturated bin, so a gap below the deepest populated
    /// bin pulls depth shallower rather than reporting a too-deep neighborhood.
    fn recalc_depth(&self, sizes: &[usize]) -> NeighborhoodDepth {
        let spec = self.identity.spec();
        let mut counts = [0u8; 32];
        for (slot, size) in counts.iter_mut().zip(sizes) {
            *slot = u8::try_from(*size).unwrap_or(u8::MAX);
        }

        // Saturation comes from the limits so the depth frontier and the
        // allocation floors can never disagree, on any construction path;
        // production threads `SwarmSpec::saturation_peers()` into the limits
        // at behaviour construction.
        let saturation = u8::try_from(self.config.limits.saturation()).unwrap_or(u8::MAX);
        let depth =
            recompute_neighborhood_depth(&counts, saturation, spec.neighborhood_low_watermark());
        NeighborhoodDepth::new(Bin::new(depth.get().min(self.max_po)).unwrap_or(Bin::MAX))
    }

    /// Total saturation deficit (in peers) across the bins below `depth`,
    /// computed from the given connected-peer bin sizes.
    ///
    /// Zero when every bin below the depth holds at least `saturation`
    /// connected peers, which is exactly the state in which `depth` was
    /// published; after disconnections it measures how many peers short of
    /// re-validating that depth the table is.
    fn saturation_deficit_below(&self, depth: NeighborhoodDepth, sizes: &[usize]) -> usize {
        let saturation = self.config.limits.saturation();
        all_bins(self.max_bin())
            .filter(|bin| !depth.contains(*bin))
            .map(|bin| saturation.saturating_sub(sizes.get(bin.as_index()).copied().unwrap_or(0)))
            .sum()
    }

    /// Recompute the instantaneous depth and fold it through the lowering
    /// hysteresis, updating the published depth.
    ///
    /// Raising (or holding) depth is applied immediately and clears any
    /// pending lower: over-connection is harmless and the trim floor
    /// protects the climb. Lowering is applied immediately only when the
    /// saturation deficit below the published depth exceeds
    /// [`DEPTH_LOWER_DEFICIT_TOLERANCE`] (real capacity loss: a bin two or
    /// more peers short, or several bins each one short). A marginal
    /// deficit, the signature of a single churning frontier peer, holds the
    /// published depth and starts (or continues) the stability window; the
    /// lower depth is published only once the instantaneous depth has
    /// stayed below the published depth for the whole window.
    ///
    /// While a lower is pending every consumer, including
    /// [`Self::evaluate_connections`] (and through it the effective-depth
    /// taper) and bin trimming, keeps seeing the held published depth, so
    /// a one-peer flap never retargets allocation.
    ///
    /// Returns `true` when the published depth was mutated, so callers that
    /// reach this without a routing-table mutation (the periodic tick) can
    /// re-anchor the neighborhood-stability clock; the connect and
    /// disconnect handlers re-anchor unconditionally for the membership
    /// change regardless of the return value.
    fn publish_depth_at(&self, now: Instant) -> bool {
        // One snapshot feeds both the depth recompute and the deficit so the
        // two can never disagree about the table state.
        let sizes = self.connected_peers.bin_sizes();
        let raw = self.recalc_depth(&sizes);
        let published = self.depth();

        if raw >= published {
            // Raise or no change: apply immediately; the table recovered,
            // so drop any pending lower.
            *self.pending_depth_lower.lock() = None;
            self.depth.store(raw.get(), Ordering::Relaxed);
            return raw != published;
        }

        if self.saturation_deficit_below(published, &sizes) > DEPTH_LOWER_DEFICIT_TOLERANCE {
            *self.pending_depth_lower.lock() = None;
            self.depth.store(raw.get(), Ordering::Relaxed);
            return true;
        }

        let mut pending = self.pending_depth_lower.lock();
        match *pending {
            None => {
                *pending = Some(now);
                false
            }
            Some(since) if now.duration_since(since) >= self.config.depth_lower_window => {
                *pending = None;
                self.depth.store(raw.get(), Ordering::Relaxed);
                true
            }
            Some(_) => false,
        }
    }

    /// Re-run the depth hysteresis against the current table.
    ///
    /// Called from the topology behaviour's periodic tick so a pending
    /// lower publishes once its stability window expires even when no
    /// further connect or disconnect events arrive. The caller observes a
    /// resulting change by comparing [`Self::depth`] before and after, the
    /// same pattern the connection handlers use.
    ///
    /// When the tick publishes a new depth there is no connect or disconnect
    /// to re-anchor the neighborhood-stability clock, so this path re-anchors
    /// it directly; otherwise the clock would measure elapsed time against
    /// the stale pre-change anchor depth.
    pub(crate) fn refresh_depth(&self) {
        self.refresh_depth_at(Instant::now());
    }

    /// [`Self::refresh_depth`] against an explicit clock, so paused-time tests
    /// can drive the stability window and re-anchoring deterministically.
    fn refresh_depth_at(&self, now: Instant) {
        if self.publish_depth_at(now) {
            self.update_neighborhood_stability();
        }
    }

    /// Log the current routing status showing bin populations.
    pub(crate) fn log_status(&self) {
        use std::fmt::Write;

        let connected_bins = self.connected_peers.bin_sizes();
        let known_bins = self.peer_manager.index().bin_sizes();
        let depth = self.depth();

        let mut bin_status = String::with_capacity(128);
        for bin in 0..=self.max_po {
            let idx = bin as usize;
            let c = connected_bins.get(idx).copied().unwrap_or(0);
            let k = known_bins.get(idx).copied().unwrap_or(0);
            if c > 0 || k > 0 {
                if !bin_status.is_empty() {
                    bin_status.push(' ');
                }
                if bin == depth.get() {
                    let _ = write!(bin_status, "[{bin}:{c}/{k}]");
                } else {
                    let _ = write!(bin_status, "{bin}:{c}/{k}");
                }
            }
        }

        let total_connected: usize = connected_bins.iter().sum();
        let total_known: usize = known_bins.iter().sum();

        if bin_status.is_empty() {
            bin_status.push_str("(empty)");
        }

        debug!(
            depth = depth.get(),
            connected = total_connected,
            known = total_known,
            bins = %bin_status,
            "kademlia routing"
        );
    }

    /// Identify peers to evict from overpopulated bins.
    ///
    /// Order: handshaking peers first (not yet established), then active peers
    /// least worth keeping. Active victims are chosen by rank, then score,
    /// then prefix diversity: `rank(overlay)` (the lowest-ranked is evicted
    /// soonest) breaks ties on the peer score, preferring to drop unreachable
    /// peers; peers tied on both are decided by [`select_trim_victims`], which
    /// prefers evicting a peer that shares a long address prefix with a
    /// retained peer so the kept set stays spread across the bin's sub-tries.
    /// `rank` is supplied by the caller, which owns the overlay->peer-id mapping
    /// and the reachability tracker; it returns any `Ord` value, so this layer
    /// stays decoupled from the rank type. The topology behaviour passes a
    /// `PeerReachability` (ordered `Unreachable < Unknown < Reachable`), or a
    /// `(PeerReachability, is_local)` tuple when local-peer trust is on: the
    /// tuple orders lexicographically so a local peer ranks above a remote of
    /// equal reachability and is evicted last.
    pub(crate) fn eviction_candidates<R: Ord>(
        &self,
        rank: impl Fn(&OverlayAddress) -> R,
    ) -> Vec<EvictionCandidate> {
        let depth = self.depth();
        let phases = self.connection_phases.read();
        let mut candidates = Vec::new();

        // Pre-group handshaking peers by bin: O(in_progress) total
        let mut handshaking_by_bin: HashMap<Bin, Vec<OverlayAddress>> = HashMap::new();
        for (overlay, phase) in phases.iter() {
            if *phase == ConnectionPhase::Handshaking {
                handshaking_by_bin
                    .entry(self.bin_for(overlay))
                    .or_default()
                    .push(*overlay);
            }
        }

        for bin in balanced_bins(depth) {
            let evictable = self.evictable_count(bin);
            let surplus = self.config.limits.surplus(bin, depth, evictable);
            if surplus == 0 {
                continue;
            }

            let mut remaining = surplus;

            // Phase 1: Handshaking peers in this bin (O(1) lookup)
            if let Some(handshaking) = handshaking_by_bin.get(&bin) {
                for overlay in handshaking.iter().take(remaining) {
                    candidates.push(EvictionCandidate {
                        overlay: *overlay,
                        bin,
                        phase: EvictionPhase::Handshaking,
                    });
                    remaining -= 1;
                }
            }

            // Phase 2: Active peers, lowest rank first, then lowest score,
            // then prefix diversity among full ties.
            if remaining > 0 {
                let active_in_bin: Vec<_> = self
                    .connected_peers
                    .peers_in_bin(bin)
                    .into_iter()
                    .map(|overlay| {
                        let rank = rank(&overlay);
                        let score = self.peer_manager.get_peer_score(&overlay).unwrap_or(0.0);
                        (overlay, rank, score)
                    })
                    .collect();

                for overlay in select_trim_victims(active_in_bin, remaining) {
                    candidates.push(EvictionCandidate {
                        overlay,
                        bin,
                        phase: EvictionPhase::Active,
                    });
                }
            }
        }

        candidates
    }

    /// The published neighborhood depth.
    ///
    /// This is the hysteresis-filtered value (see [`Self::publish_depth_at`]):
    /// while a marginal lowering is pending its stability window the held,
    /// previous depth is returned. The raw instantaneous recompute is never
    /// exposed outside this type.
    pub(crate) fn depth(&self) -> NeighborhoodDepth {
        NeighborhoodDepth::new(Bin::new(self.depth.load(Ordering::Relaxed)).unwrap_or(Bin::MAX))
    }

    /// Connected peers in bins at and above `depth`.
    fn neighborhood_connected(&self, depth: NeighborhoodDepth) -> usize {
        neighborhood_bins(depth, self.max_bin())
            .map(|bin| self.connected_peers.bin_size(bin))
            .sum()
    }

    /// Re-anchor the neighborhood-stability clock from current state.
    ///
    /// Called after every routing-table mutation, so a saturation dip
    /// between two reads can never go unobserved. The neighborhood counts
    /// as saturated while a depth boundary is established (depth above
    /// zero) and the bins it contains collectively hold at least the
    /// saturation threshold in connected peers; the threshold is the same
    /// one the depth frontier derives from (see [`Self::recalc_depth`]),
    /// so the clock and the depth can never disagree about saturation.
    ///
    /// The clock survives mutations that keep both the depth and the
    /// saturated state unchanged; a depth change restarts it and a dip
    /// below saturation clears it.
    fn update_neighborhood_stability(&self) {
        let depth = self.depth();
        let saturated = depth > NeighborhoodDepth::ZERO
            && self.neighborhood_connected(depth) >= self.config.limits.saturation();

        let mut state = self.neighborhood_stability.lock();
        if !saturated {
            *state = None;
        } else if state.as_ref().is_none_or(|stable| stable.depth != depth) {
            *state = Some(NeighborhoodStable {
                depth,
                since: Instant::now(),
            });
        }
    }

    /// How long the neighborhood has been continuously saturated at an
    /// unchanged depth, or `None` while it is below saturation.
    pub(crate) fn neighborhood_stable_for(&self) -> Option<Duration> {
        self.neighborhood_stability
            .lock()
            .as_ref()
            .map(|stable| stable.since.elapsed())
    }

    /// The configured window [`Self::neighborhood_stable_for`] must reach
    /// before the neighborhood counts as ready for pull-syncing.
    pub(crate) fn neighborhood_stability_window(&self) -> Duration {
        self.config.neighborhood_stability_window
    }

    /// Whether the neighborhood is saturated: a depth boundary is
    /// established and the bins inside it together hold at least the
    /// saturation threshold in connected peers, the same condition as
    /// [`crate::ReadinessSnapshot::is_saturated`].
    fn neighborhood_saturated(&self, depth: NeighborhoodDepth) -> bool {
        if depth == NeighborhoodDepth::ZERO {
            return false;
        }
        self.neighborhood_connected(depth) >= self.config.limits.saturation()
    }

    /// Re-derive the topology phase from the published depth and current
    /// neighborhood saturation, committing a transition when it moved.
    ///
    /// Driven from the depth-publication points (peer connect and
    /// disconnect through the behaviour) and from the periodic connection
    /// evaluator, which alone observes the time-based `Converging` to
    /// `Stable` settle. On transition this logs one operator-facing line
    /// and records the phase metrics; callers broadcast the matching
    /// [`crate::TopologyEvent::PhaseChanged`] where they hold the event
    /// channel.
    pub(crate) fn evaluate_phase(&self) -> Option<PhaseTransition> {
        let depth = self.depth();
        let saturated = self.neighborhood_saturated(depth);
        let mut tracker = self.topology_phase.lock();
        let transition = tracker.evaluate(depth, saturated, PhaseInstant::now())?;

        // Record while the tracker lock is held so concurrent evaluations
        // committing back-to-back transitions cannot interleave their logs
        // and gauge updates out of order.
        info!(
            from = %transition.from,
            to = %transition.to,
            depth = transition.depth.get(),
            time_in_phase = ?transition.time_in_phase,
            "topology phase changed"
        );
        record_topology_phase_change(transition.from, transition.to);
        Some(transition)
    }

    /// Current topology phase and the time spent in it.
    pub(crate) fn phase_status(&self) -> (TopologyPhase, Duration) {
        let tracker = self.topology_phase.lock();
        (tracker.phase(), tracker.time_in_phase(PhaseInstant::now()))
    }

    /// Connected peers in the neighborhood (bins >= depth).
    pub(crate) fn neighbors(&self, depth: NeighborhoodDepth) -> Vec<OverlayAddress> {
        let mut result = Vec::new();
        for bin in neighborhood_bins(depth, self.max_bin()) {
            result.extend(self.connected_peers.peers_in_bin(bin));
        }
        result
    }

    /// Top `count` connected peers closest to `address` by proximity.
    pub(crate) fn closest_to(&self, address: &ChunkAddress, count: usize) -> Vec<OverlayAddress> {
        let mut peers_with_distance: Vec<_> = self
            .connected_peers
            .all_peers()
            .into_iter()
            .map(|peer| {
                let proximity = address.proximity(&peer);
                (peer, proximity)
            })
            .collect();

        if count < peers_with_distance.len() {
            // O(n) partition to find the top-k elements
            peers_with_distance.select_nth_unstable_by(count, |a, b| b.1.cmp(&a.1));
            peers_with_distance.truncate(count);
        }
        // Sort just the top-k: O(k log k)
        peers_with_distance.sort_by_key(|b| std::cmp::Reverse(b.1));

        peers_with_distance
            .into_iter()
            .map(|(peer, _)| peer)
            .collect()
    }

    pub(crate) fn bin_sizes(&self) -> Vec<(usize, usize)> {
        let connected = self.connected_peers.bin_sizes();
        let known = self.peer_manager.index().bin_sizes();
        connected.into_iter().zip(known).collect()
    }

    /// Get connected and known peer counts for a single bin.
    pub(crate) fn bin_peer_counts(&self, bin: Bin) -> (usize, usize) {
        (
            self.connected_peers.bin_size(bin),
            self.peer_manager.index().bin_size(bin),
        )
    }

    /// Returns (dialing, handshaking, active) counts for bin.
    pub(crate) fn bin_phase_counts(&self, bin: Bin) -> (usize, usize, usize) {
        (
            atomic_load(&self.dialing_counts, bin),
            atomic_load(&self.handshaking_counts, bin),
            atomic_load(&self.active_counts, bin),
        )
    }

    /// Phase counts for all bins: (bin, dialing, handshaking, active).
    pub(crate) fn all_bin_phases(&self) -> Vec<(Bin, usize, usize, usize)> {
        all_bins(self.max_bin())
            .map(|bin| {
                let (d, h, a) = self.bin_phase_counts(bin);
                (bin, d, h, a)
            })
            .collect()
    }

    /// Get total known peers count (from PeerManager).
    pub(crate) fn known_peers_total(&self) -> usize {
        self.peer_manager.index().len()
    }

    /// Get total connected peers count.
    pub(crate) fn connected_peers_total(&self) -> usize {
        self.connected_peers.len()
    }

    /// Connected peers whose handshake-confirmed node type stores chunks.
    ///
    /// Walks the connected-peer index (bounded by the connection targets)
    /// and resolves each node type from the peer manager, the authoritative
    /// holder of handshake-confirmed types.
    pub(crate) fn connected_storer_total(&self) -> usize {
        self.connected_peers
            .all_peers()
            .into_iter()
            .filter(|overlay| {
                self.peer_manager
                    .node_type(overlay)
                    .is_some_and(|node_type| node_type.requires_storage())
            })
            .count()
    }

    pub(crate) fn connected_overlays_in_bin(&self, bin: Bin) -> Vec<OverlayAddress> {
        self.connected_peers.peers_in_bin(bin)
    }

    fn peer_connected(&self, peer: OverlayAddress) {
        let bin = self.bin_for(&peer);

        if self.connected_peers.add(peer).is_ok() {
            let old_depth = self.depth();
            self.publish_depth_at(Instant::now());
            let new_depth = self.depth();
            self.update_neighborhood_stability();

            debug!(
                %peer,
                bin = bin.get(),
                depth = new_depth.get(),
                connected = self.connected_peers.len(),
                "peer connected"
            );

            if new_depth != old_depth {
                info!(
                    old_depth = old_depth.get(),
                    new_depth = new_depth.get(),
                    "kademlia depth changed"
                );
                self.log_status();
            }
        }
    }

    fn peer_disconnected(&self, peer: &OverlayAddress) {
        if self.connected_peers.remove(peer) {
            let bin = self.bin_for(peer);

            let old_depth = self.depth();
            self.publish_depth_at(Instant::now());
            let new_depth = self.depth();
            self.update_neighborhood_stability();

            debug!(
                %peer,
                bin = bin.get(),
                depth = new_depth.get(),
                connected = self.connected_peers.len(),
                "peer disconnected"
            );

            if new_depth != old_depth {
                info!(
                    old_depth = old_depth.get(),
                    new_depth = new_depth.get(),
                    "kademlia depth changed"
                );
                self.log_status();
            }
        }
    }
}

impl<I: SwarmIdentity> RoutingCapacity for KademliaRouting<I> {
    fn try_reserve_dial(&self, overlay: &OverlayAddress, _node_type: SwarmNodeType) -> bool {
        let bin = self.bin_for(overlay);
        let effective = self.effective_count(bin);

        let mut phases = self.connection_phases.write();

        if phases.contains_key(overlay) {
            return false;
        }

        // Use depth-aware limits for capacity decision
        if !self.config.limits.needs_more(bin, self.depth(), effective) {
            return false;
        }

        atomic_inc(&self.dialing_counts, bin);
        phases.insert(*overlay, ConnectionPhase::Dialing);
        record_phase_transition(phase::NONE, phase::DIALING);
        true
    }

    fn release_dial(&self, overlay: &OverlayAddress) {
        let mut phases = self.connection_phases.write();
        if let Some(ConnectionPhase::Dialing) = phases.remove(overlay) {
            let bin = self.bin_for(overlay);
            atomic_dec(&self.dialing_counts, bin);
            record_phase_transition(phase::DIALING, phase::NONE);
        }
    }

    fn dial_connected(&self, overlay: &OverlayAddress) {
        let bin = self.bin_for(overlay);
        let mut phases = self.connection_phases.write();

        if let Some(phase) = phases.get_mut(overlay)
            && *phase == ConnectionPhase::Dialing
        {
            atomic_dec(&self.dialing_counts, bin);
            atomic_inc(&self.handshaking_counts, bin);
            *phase = ConnectionPhase::Handshaking;
            record_phase_transition(phase::DIALING, phase::HANDSHAKING);
        }
    }

    fn handshake_completed(&self, overlay: &OverlayAddress) {
        let bin = self.bin_for(overlay);
        let mut phases = self.connection_phases.write();

        if let Some(phase) = phases.get_mut(overlay)
            && *phase == ConnectionPhase::Handshaking
        {
            atomic_dec(&self.handshaking_counts, bin);
            atomic_inc(&self.active_counts, bin);
            *phase = ConnectionPhase::Active;
            record_phase_transition(phase::HANDSHAKING, phase::ACTIVE);
        }
    }

    fn release_handshake(&self, overlay: &OverlayAddress) {
        let mut phases = self.connection_phases.write();
        if let Some(ConnectionPhase::Handshaking) = phases.remove(overlay) {
            let bin = self.bin_for(overlay);
            atomic_dec(&self.handshaking_counts, bin);
            record_phase_transition(phase::HANDSHAKING, phase::NONE);
        }
    }

    fn disconnected(&self, overlay: &OverlayAddress) {
        let mut phases = self.connection_phases.write();
        if let Some(phase) = phases.remove(overlay) {
            let bin = self.bin_for(overlay);
            match phase {
                ConnectionPhase::Dialing => {
                    atomic_dec(&self.dialing_counts, bin);
                    record_phase_transition(phase::DIALING, phase::NONE);
                }
                ConnectionPhase::Handshaking => {
                    atomic_dec(&self.handshaking_counts, bin);
                    record_phase_transition(phase::HANDSHAKING, phase::NONE);
                }
                ConnectionPhase::Active => {
                    atomic_dec(&self.active_counts, bin);
                    record_phase_transition(phase::ACTIVE, phase::NONE);
                }
            }
        }
    }

    fn should_accept_inbound(&self, overlay: &OverlayAddress, _node_type: SwarmNodeType) -> bool {
        let bin = self.bin_for(overlay);
        let effective = self.effective_count(bin);

        let phases = vertex_observability::timed_read(
            &self.connection_phases,
            metrics::histogram!("topology_routing_phases_lock_seconds"),
        );
        !phases.contains_key(overlay)
            && self
                .config
                .limits
                .should_accept_inbound(bin, self.depth(), effective)
    }

    fn reserve_inbound(&self, overlay: &OverlayAddress) {
        let bin = self.bin_for(overlay);
        let mut phases = self.connection_phases.write();

        if !phases.contains_key(overlay) {
            atomic_inc(&self.handshaking_counts, bin);
            phases.insert(*overlay, ConnectionPhase::Handshaking);
            record_phase_transition(phase::NONE, phase::HANDSHAKING);
        }
    }
}

impl<I: SwarmIdentity> SwarmRouting<I> for KademliaRouting<I> {
    fn connected(&self, peer: OverlayAddress) {
        self.peer_connected(peer);
    }

    fn on_peer_disconnected(&self, peer: &OverlayAddress) {
        self.peer_disconnected(peer);
    }

    fn remove_peer(&self, peer: &OverlayAddress) {
        self.connected_peers.remove(peer);
        self.update_neighborhood_stability();
        debug!(%peer, "removed peer from routing");
    }
}

// Methods internalized from the poll loop — called only by the background evaluator task
// and topology behaviour within the routing module.
impl<I: SwarmIdentity + 'static> KademliaRouting<I> {
    /// Pop the next pending dial candidate, highest bin first (called from
    /// the poll loop's rate-shaped drain). O(bins).
    pub(crate) fn pop_candidate(&self) -> Option<OverlayAddress> {
        self.candidate_queues.pop_next()
    }

    /// Return a popped candidate to its bin queue, e.g. when the dial-rate
    /// bucket ran out before it could be dialed. Re-enters the dedup set so
    /// the evaluator does not select it again while it waits.
    pub(crate) fn requeue_candidate(&self, peer: OverlayAddress) {
        let bin = self.bin_for(&peer);
        let _ = self.candidate_queues.push(bin, peer);
    }

    /// Evaluate connections and enqueue candidates into per-bin queues.
    #[tracing::instrument(skip(self), level = "debug")]
    pub(crate) fn evaluate_connections(&self) {
        // Use effective depth (max of connected and estimated) for allocation
        let connected_depth = self.depth();
        let known_bin_sizes = self.peer_manager.index().bin_sizes();
        let effective_depth = self
            .config
            .limits
            .effective_depth(connected_depth, &known_bin_sizes);

        if effective_depth != connected_depth {
            trace!(
                connected_depth = connected_depth.get(),
                effective_depth = effective_depth.get(),
                "using estimated depth for allocation"
            );
        }

        // Capture state using effective depth — no mutation of shared limits
        let snapshot = self.capture_candidate_state(effective_depth);
        let mut selector = CandidateSelector::new(
            &snapshot,
            &self.connected_peers,
            self.config.max_neighbor_candidates + self.config.max_balanced_candidates,
        );

        select_neighborhood_candidates(
            &mut selector,
            &self.peer_manager,
            |bin| self.effective_count(bin),
            self.max_bin(),
        );
        let neighbor_candidates = selector.len();

        select_balanced_candidates(&mut selector, &self.peer_manager, |bin| {
            self.effective_count(bin)
        });
        let balanced_candidates = selector.len() - neighbor_candidates;

        let new_candidates = selector.finish();

        let mut added = 0usize;
        for c in new_candidates {
            let bin = self.bin_for(&c);
            if self.candidate_queues.push(bin, c) {
                added += 1;
            }
        }

        if added > 0 {
            debug!(
                added,
                neighbors = neighbor_candidates,
                balanced = balanced_candidates,
                "evaluated connection candidates"
            );
        } else {
            trace!("no new connection candidates");
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use super::*;
    use nectar_primitives::SwarmAddress;
    use vertex_swarm_peer_manager::PeerManagerConfig;
    use vertex_swarm_test_utils::{MockIdentity, make_swarm_peer_minimal};

    fn b(n: u8) -> Bin {
        Bin::new(n).expect("valid bin")
    }

    fn d(n: u8) -> NeighborhoodDepth {
        NeighborhoodDepth::new(b(n))
    }

    fn make_routing(
        base: OverlayAddress,
        config: KademliaConfig,
    ) -> (
        Arc<KademliaRouting<MockIdentity>>,
        Arc<PeerManager<MockIdentity>>,
    ) {
        let identity = MockIdentity::with_overlay(base);
        let peer_manager = PeerManager::new(&identity, PeerManagerConfig::default());
        let routing = KademliaRouting::new(identity, config, peer_manager.clone());
        (routing, peer_manager)
    }

    #[test]
    fn test_routing_creation() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default();
        let (routing, _pm) = make_routing(base, config);

        assert_eq!(routing.depth().get(), 0);
        assert_eq!(routing.connected_peers.len(), 0);
    }

    #[test]
    fn test_add_and_connect_peers() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default();
        let (routing, pm) = make_routing(base, config);

        let peer1 = SwarmAddress::with_first_byte(0x80);
        let peer2 = SwarmAddress::with_first_byte(0x40);

        // Add peers via PeerManager (not routing.add_peers which is now no-op)
        pm.store_discovered_peer(make_swarm_peer_minimal(0x80));
        pm.store_discovered_peer(make_swarm_peer_minimal(0x40));
        assert_eq!(pm.index().len(), 2);
        assert_eq!(routing.connected_peers.len(), 0);

        SwarmRouting::connected(&*routing, peer1);
        assert_eq!(routing.connected_peers.len(), 1);

        SwarmRouting::connected(&*routing, peer2);
        assert_eq!(routing.connected_peers.len(), 2);
    }

    #[test]
    fn test_capacity_reserve_and_release() {
        let base = SwarmAddress::with_first_byte(0x00);
        // Pin the depth-0 bootstrap target to 2 so the capacity mechanism is
        // exercised with small numbers (default bootstrap fill is generous).
        let config = KademliaConfig::default()
            .with_nominal(2)
            .with_bootstrap_target(2)
            .with_saturation(2);
        let (routing, _pm) = make_routing(base, config);

        let peer1 = SwarmAddress::with_first_byte(0x80); // bin=0
        let peer2 = SwarmAddress::with_first_byte(0xc0); // bin=0
        let peer3 = SwarmAddress::with_first_byte(0xa0); // bin=0

        // First reserve succeeds (effective=0 < target=2)
        assert!(routing.try_reserve_dial(&peer1, SwarmNodeType::Storer));

        // Second reserve succeeds (effective=1 < nominal=2)
        assert!(routing.try_reserve_dial(&peer2, SwarmNodeType::Storer));

        // Third fails (effective=2 >= nominal=2)
        assert!(!routing.try_reserve_dial(&peer3, SwarmNodeType::Storer));

        // Release one
        routing.release_dial(&peer1);

        // Now third succeeds (effective=1 < nominal=2)
        assert!(routing.try_reserve_dial(&peer3, SwarmNodeType::Storer));
    }

    #[test]
    fn test_capacity_state_transitions() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default().with_nominal(2);
        let (routing, _pm) = make_routing(base, config);

        let peer = SwarmAddress::with_first_byte(0x80); // bin=0

        // Reserve dial
        assert!(routing.try_reserve_dial(&peer, SwarmNodeType::Storer));
        assert_eq!(routing.effective_count(b(0)), 1);

        // Transition to handshaking
        routing.dial_connected(&peer);
        assert_eq!(routing.effective_count(b(0)), 1);

        // Transition to active
        routing.handshake_completed(&peer);
        assert_eq!(routing.effective_count(b(0)), 1);
        assert_eq!(atomic_load(&routing.active_counts, b(0)), 1);

        // Disconnect
        RoutingCapacity::disconnected(&*routing, &peer);
        assert_eq!(routing.effective_count(b(0)), 0);
    }

    #[test]
    fn test_disconnect_and_reconnect() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default();
        let (routing, pm) = make_routing(base, config);

        let peer = SwarmAddress::with_first_byte(0x80);

        // Add peer to PeerManager
        pm.store_discovered_peer(make_swarm_peer_minimal(0x80));

        SwarmRouting::connected(&*routing, peer);
        assert!(routing.connected_peers.exists(&peer));
        // Peer still in PeerManager
        assert!(pm.index().exists(&peer));

        SwarmRouting::on_peer_disconnected(&*routing, &peer);
        assert!(!routing.connected_peers.exists(&peer));
        // Peer still in PeerManager after disconnect
        assert!(pm.index().exists(&peer));
    }

    /// Build an overlay at exactly proximity order `bin` to base `0x00`,
    /// disambiguated by `idx` in a deep byte (does not affect proximity order).
    fn addr_in_bin(bin: u8, idx: u8) -> OverlayAddress {
        let mut b = [0u8; 32];
        b[(bin / 8) as usize] = 0x80 >> (bin % 8);
        b[31] = idx;
        OverlayAddress::from(b)
    }

    #[test]
    fn test_depth_stays_zero_below_saturation() {
        // A handful of peers in one mid bin, everything shallower empty. The
        // depth frontier is the shallowest unsaturated bin (0), so depth stays
        // 0 - the corrected algorithm does not jump to the deepest populated
        // bin the way the old deepest-bin scan did.
        let base = SwarmAddress::with_first_byte(0x00);
        let (routing, _pm) = make_routing(base, KademliaConfig::default());

        assert_eq!(routing.depth().get(), 0);
        for idx in 0..2 {
            SwarmRouting::connected(&*routing, addr_in_bin(5, idx));
        }
        assert_eq!(routing.depth().get(), 0);
    }

    #[test]
    fn test_depth_caps_at_gap() {
        // Regression for the deepest-bin bug: bin 0 is saturated, bins 1..8 are
        // empty, and bin 8 holds several peers. The old scan reported depth 8;
        // the corrected algorithm caps depth at the shallowest unsaturated bin
        // (bin 1, just past the saturated frontier at bin 0).
        let base = SwarmAddress::with_first_byte(0x00);
        let (routing, _pm) = make_routing(base, KademliaConfig::default());

        for idx in 0..8 {
            SwarmRouting::connected(&*routing, addr_in_bin(0, idx));
        }
        for idx in 0..5 {
            SwarmRouting::connected(&*routing, addr_in_bin(8, idx));
        }

        assert_eq!(
            routing.depth().get(),
            1,
            "gap below the deep bin must cap depth"
        );
    }

    #[test]
    fn test_depth_climbs_with_saturated_bins() {
        // Bins 0,1,2 saturated (>= 8) with bin 3 holding the low-watermark (3)
        // anchors the neighborhood at bin 3.
        let base = SwarmAddress::with_first_byte(0x00);
        let (routing, _pm) = make_routing(base, KademliaConfig::default());

        for bin in 0..3 {
            for idx in 0..8 {
                SwarmRouting::connected(&*routing, addr_in_bin(bin, idx));
            }
        }
        for idx in 0..3 {
            SwarmRouting::connected(&*routing, addr_in_bin(3, idx));
        }

        assert_eq!(routing.depth().get(), 3);
    }

    /// Bin 0 saturated, bins 1..=3 holding 5 + 3 + 1 peers: depth anchors at
    /// 1 and the neighborhood (bins at and above 1) holds 9 connected.
    fn saturate_to_depth_one(routing: &KademliaRouting<MockIdentity>) {
        for idx in 0..8 {
            SwarmRouting::connected(routing, addr_in_bin(0, idx));
        }
        for idx in 0..5 {
            SwarmRouting::connected(routing, addr_in_bin(1, idx));
        }
        for idx in 0..3 {
            SwarmRouting::connected(routing, addr_in_bin(2, idx));
        }
        SwarmRouting::connected(routing, addr_in_bin(3, 0));
    }

    #[test]
    fn test_neighborhood_stability_tracks_saturation() {
        let base = SwarmAddress::with_first_byte(0x00);
        let (routing, _pm) = make_routing(base, KademliaConfig::default());

        assert!(
            routing.neighborhood_stable_for().is_none(),
            "empty table is not saturated"
        );

        saturate_to_depth_one(&routing);
        assert_eq!(routing.depth().get(), 1);
        assert!(
            routing.neighborhood_stable_for().is_some(),
            "saturated neighborhood must carry a stability clock"
        );

        // Disconnecting two neighborhood peers (9 -> 7) drops below the
        // saturation threshold (8) and clears the clock.
        SwarmRouting::on_peer_disconnected(&*routing, &addr_in_bin(1, 0));
        SwarmRouting::on_peer_disconnected(&*routing, &addr_in_bin(1, 1));
        assert!(routing.neighborhood_stable_for().is_none());
    }

    #[test]
    fn test_neighborhood_stability_cleared_by_remove_peer() {
        // remove_peer (the ban path) bypasses the disconnect bookkeeping but
        // still shrinks the neighborhood; the clock must observe it.
        let base = SwarmAddress::with_first_byte(0x00);
        let (routing, _pm) = make_routing(base, KademliaConfig::default());

        saturate_to_depth_one(&routing);
        assert!(routing.neighborhood_stable_for().is_some());

        SwarmRouting::remove_peer(&*routing, &addr_in_bin(1, 0));
        SwarmRouting::remove_peer(&*routing, &addr_in_bin(1, 1));
        assert!(routing.neighborhood_stable_for().is_none());
    }

    /// A depth lowering published by the periodic tick (no connect or
    /// disconnect event) must re-anchor the neighborhood-stability clock
    /// against the newly published depth, not leave it measuring against the
    /// stale pre-change anchor.
    #[test]
    fn test_refresh_depth_reanchors_neighborhood_stability() {
        // bin0=8, bin1=5, bin2=3, bin3=1: depth anchors at 1 and the
        // neighborhood (bins >= 1, holding 9) is saturated, so the stability
        // clock is anchored at depth 1.
        let base = SwarmAddress::with_first_byte(0x00);
        let (routing, _pm) = make_routing(base, KademliaConfig::default());
        saturate_to_depth_one(&routing);
        assert_eq!(routing.depth().get(), 1);
        let anchored_at_depth_1 = routing
            .neighborhood_stability
            .lock()
            .as_ref()
            .map(|stable| stable.depth);
        assert_eq!(
            anchored_at_depth_1,
            Some(routing.depth()),
            "clock must start anchored at the depth-1 neighborhood"
        );

        // Drop one peer from bin 0 (below depth): a single-peer deficit, so
        // the lower depth is deferred and the published depth holds at 1 with
        // a pending window. No connection event publishes it.
        SwarmRouting::on_peer_disconnected(&*routing, &addr_in_bin(0, 0));
        assert_eq!(
            routing.depth().get(),
            1,
            "single-peer deficit must defer the lower"
        );
        assert!(routing.pending_depth_lower.lock().is_some());

        // The periodic tick fires after the window expires and publishes the
        // lower depth (0). The clock must observe the change even though no
        // connect or disconnect occurred: at depth 0 the neighborhood is no
        // longer saturated, so the clock clears rather than staying anchored
        // at the stale depth 1.
        let after_window = Instant::now() + routing.config.depth_lower_window;
        routing.refresh_depth_at(after_window);
        assert_eq!(
            routing.depth().get(),
            0,
            "expired window must publish the lower depth through the tick"
        );
        assert!(
            routing.neighborhood_stability.lock().is_none(),
            "the tick-driven lower must re-anchor (here clear) the stability clock"
        );
    }

    /// Depth-3 fixture: bins 0..=2 saturated (8 peers each, the default
    /// saturation), bin 3 holding the low watermark (3 peers).
    fn routing_at_depth_3() -> Arc<KademliaRouting<MockIdentity>> {
        let base = SwarmAddress::with_first_byte(0x00);
        let (routing, _pm) = make_routing(base, KademliaConfig::default());
        for bin in 0..3 {
            for idx in 0..8 {
                SwarmRouting::connected(&*routing, addr_in_bin(bin, idx));
            }
        }
        for idx in 0..3 {
            SwarmRouting::connected(&*routing, addr_in_bin(3, idx));
        }
        assert_eq!(routing.depth().get(), 3, "fixture must start at depth 3");
        routing
    }

    #[test]
    fn test_depth_lower_deferred_on_single_peer_deficit() {
        // One disconnect in a frontier bin sitting exactly at saturation
        // leaves a single-peer deficit: the published depth holds and the
        // stability window starts.
        let routing = routing_at_depth_3();

        SwarmRouting::on_peer_disconnected(&*routing, &addr_in_bin(1, 0));

        assert_eq!(
            routing.depth().get(),
            3,
            "single-peer deficit must not lower the published depth"
        );
        assert!(
            routing.pending_depth_lower.lock().is_some(),
            "a stability window must be pending"
        );
    }

    #[test]
    fn test_depth_recovery_within_window_publishes_nothing() {
        let routing = routing_at_depth_3();
        let peer = addr_in_bin(1, 0);

        SwarmRouting::on_peer_disconnected(&*routing, &peer);
        assert_eq!(routing.depth().get(), 3);

        // The bin refills before the window expires: pending lower clears.
        SwarmRouting::connected(&*routing, peer);
        assert_eq!(routing.depth().get(), 3);
        assert!(
            routing.pending_depth_lower.lock().is_none(),
            "recovery must clear the pending lower"
        );

        // Even a refresh long after the original window would have expired
        // publishes nothing.
        let after_window = Instant::now() + routing.config.depth_lower_window;
        routing.publish_depth_at(after_window);
        assert_eq!(routing.depth().get(), 3);
    }

    #[test]
    fn test_depth_lowers_immediately_on_two_peer_deficit() {
        // Two peers gone from the same frontier bin (saturation - 2) is real
        // capacity loss: the lower depth publishes with no window.
        let routing = routing_at_depth_3();

        SwarmRouting::on_peer_disconnected(&*routing, &addr_in_bin(1, 0));
        SwarmRouting::on_peer_disconnected(&*routing, &addr_in_bin(1, 1));

        assert_eq!(routing.depth().get(), 1);
        assert!(routing.pending_depth_lower.lock().is_none());
    }

    #[test]
    fn test_depth_lowers_immediately_when_two_bins_fall_below() {
        // Two frontier bins each one peer short also exceeds the one-peer
        // tolerance: the lower depth publishes immediately.
        let routing = routing_at_depth_3();

        SwarmRouting::on_peer_disconnected(&*routing, &addr_in_bin(0, 0));
        assert_eq!(routing.depth().get(), 3, "first loss is deferred");

        SwarmRouting::on_peer_disconnected(&*routing, &addr_in_bin(1, 0));
        assert_eq!(routing.depth().get(), 0);
        assert!(routing.pending_depth_lower.lock().is_none());
    }

    #[test]
    fn test_depth_lower_publishes_after_window_expiry() {
        let routing = routing_at_depth_3();

        SwarmRouting::on_peer_disconnected(&*routing, &addr_in_bin(1, 0));
        assert_eq!(routing.depth().get(), 3);

        // Re-checks inside the window keep holding the published depth.
        routing.publish_depth_at(Instant::now());
        assert_eq!(routing.depth().get(), 3);

        // A refresh after the window expires publishes the lower depth.
        let after_window = Instant::now() + routing.config.depth_lower_window;
        routing.publish_depth_at(after_window);
        assert_eq!(
            routing.depth().get(),
            1,
            "expired window must publish the lower depth"
        );
        assert!(routing.pending_depth_lower.lock().is_none());
    }

    #[test]
    fn test_churn_flap_never_changes_published_depth() {
        // Alternating disconnect/reconnect of one frontier peer (classic
        // churn) must never flap the published depth.
        let routing = routing_at_depth_3();
        let churner = addr_in_bin(1, 0);

        for _ in 0..10 {
            SwarmRouting::on_peer_disconnected(&*routing, &churner);
            assert_eq!(routing.depth().get(), 3);
            SwarmRouting::connected(&*routing, churner);
            assert_eq!(routing.depth().get(), 3);
        }
        assert!(routing.pending_depth_lower.lock().is_none());
    }

    #[test]
    fn test_phase_starts_bootstrap_and_converges_on_depth_climb() {
        let base = SwarmAddress::with_first_byte(0x00);
        let (routing, _pm) = make_routing(base, KademliaConfig::default());

        assert_eq!(routing.phase_status().0, TopologyPhase::Bootstrap);
        assert!(
            routing.evaluate_phase().is_none(),
            "no transition while depth stays 0"
        );

        // Same shape as test_depth_climbs_with_saturated_bins: depth -> 3.
        for bin in 0..3 {
            for idx in 0..8 {
                SwarmRouting::connected(&*routing, addr_in_bin(bin, idx));
            }
        }
        for idx in 0..3 {
            SwarmRouting::connected(&*routing, addr_in_bin(3, idx));
        }
        assert_eq!(routing.depth().get(), 3);

        let transition = routing.evaluate_phase().expect("depth climb transitions");
        assert_eq!(transition.from, TopologyPhase::Bootstrap);
        assert_eq!(transition.to, TopologyPhase::Converging);
        assert_eq!(transition.depth.get(), 3);

        assert!(
            routing.evaluate_phase().is_none(),
            "re-evaluation without state change commits nothing"
        );
        assert_eq!(routing.phase_status().0, TopologyPhase::Converging);
    }

    #[test]
    fn test_phase_full_lifecycle_with_zero_window() {
        // A zero stability window makes the time gate pass immediately, so
        // the saturation condition alone drives Converging vs Stable and
        // the lifecycle is testable without simulated clocks.
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default().with_phase_stability_window(Duration::ZERO);
        let (routing, _pm) = make_routing(base, config);

        // Saturate bin 0 (depth anchors at 1) and hold 9 connected peers in
        // the neighborhood, above the saturation threshold of 8.
        for idx in 0..8 {
            SwarmRouting::connected(&*routing, addr_in_bin(0, idx));
        }
        for idx in 0..5 {
            SwarmRouting::connected(&*routing, addr_in_bin(1, idx));
        }
        for idx in 0..3 {
            SwarmRouting::connected(&*routing, addr_in_bin(2, idx));
        }
        SwarmRouting::connected(&*routing, addr_in_bin(3, 0));
        assert_eq!(routing.depth().get(), 1);

        let transition = routing.evaluate_phase().expect("saturated neighborhood");
        assert_eq!(transition.to, TopologyPhase::Stable);

        // Losing neighborhood saturation (9 -> 7 connected at depth 1)
        // falls back to Converging.
        SwarmRouting::on_peer_disconnected(&*routing, &addr_in_bin(1, 3));
        SwarmRouting::on_peer_disconnected(&*routing, &addr_in_bin(1, 4));
        assert_eq!(routing.depth().get(), 1);
        let transition = routing.evaluate_phase().expect("saturation lost");
        assert_eq!(transition.from, TopologyPhase::Stable);
        assert_eq!(transition.to, TopologyPhase::Converging);

        // Depth collapsing to 0 falls all the way back to Bootstrap.
        for idx in 0..8 {
            SwarmRouting::on_peer_disconnected(&*routing, &addr_in_bin(0, idx));
        }
        assert_eq!(routing.depth().get(), 0);
        let transition = routing.evaluate_phase().expect("depth collapsed");
        assert_eq!(transition.to, TopologyPhase::Bootstrap);
    }

    #[test]
    fn test_closest_to() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default();
        let (routing, _pm) = make_routing(base, config);

        let peer_po0 = SwarmAddress::with_first_byte(0x80);
        let peer_po1 = SwarmAddress::with_first_byte(0x40);
        let peer_po2 = SwarmAddress::with_first_byte(0x20);

        SwarmRouting::connected(&*routing, peer_po0);
        SwarmRouting::connected(&*routing, peer_po1);
        SwarmRouting::connected(&*routing, peer_po2);

        let mut target_bytes = [0x00u8; 32];
        target_bytes[0] = 0x21;
        let target = ChunkAddress::from(target_bytes);

        let closest = routing.closest_to(&target, 2);
        assert_eq!(closest.len(), 2);
        assert_eq!(closest[0], peer_po2);
    }

    #[test]
    fn test_neighbors() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default().with_nominal(1);
        let (routing, _pm) = make_routing(base, config);

        let peer_po0 = SwarmAddress::with_first_byte(0x80);
        let peer_po1 = SwarmAddress::with_first_byte(0x40);
        let peer_po5 = {
            let mut bytes = [0x00u8; 32];
            bytes[0] = 0x04;
            OverlayAddress::from(bytes)
        };

        SwarmRouting::connected(&*routing, peer_po0);
        SwarmRouting::connected(&*routing, peer_po1);
        SwarmRouting::connected(&*routing, peer_po5);

        let neighbors_d0 = routing.neighbors(d(0));
        assert_eq!(neighbors_d0.len(), 3);

        let neighbors_d2 = routing.neighbors(d(2));
        assert_eq!(neighbors_d2.len(), 1);
        assert_eq!(neighbors_d2[0], peer_po5);
    }

    #[test]
    fn test_inbound_capacity() {
        let base = SwarmAddress::with_first_byte(0x00);
        // With bootstrap and oversaturation pinned to 2 and headroom 0, the
        // depth-0 inbound ceiling = 2
        let config = KademliaConfig::default()
            .with_nominal(2)
            .with_inbound_headroom(0)
            .with_bootstrap_target(2)
            .with_oversaturation_peers(2)
            .with_saturation(2);
        let (routing, _pm) = make_routing(base, config);

        let peer1 = SwarmAddress::with_first_byte(0x80);
        let peer2 = SwarmAddress::with_first_byte(0xc0);
        let peer3 = SwarmAddress::with_first_byte(0xa0);

        // Can accept first inbound
        assert!(routing.should_accept_inbound(&peer1, SwarmNodeType::Storer));
        routing.reserve_inbound(&peer1);

        // Can accept second inbound
        assert!(routing.should_accept_inbound(&peer2, SwarmNodeType::Storer));
        routing.reserve_inbound(&peer2);

        // At capacity (effective=2 >= target+headroom=2)
        assert!(!routing.should_accept_inbound(&peer3, SwarmNodeType::Storer));

        // Complete one handshake
        routing.handshake_completed(&peer1);

        // Still at capacity (peer1 now active)
        assert!(!routing.should_accept_inbound(&peer3, SwarmNodeType::Storer));

        // Disconnect peer1
        RoutingCapacity::disconnected(&*routing, &peer1);

        // Now can accept
        assert!(routing.should_accept_inbound(&peer3, SwarmNodeType::Storer));
    }

    #[test]
    fn test_depth_aware_targets() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default();
        let (routing, _pm) = make_routing(base, config);

        // Initially depth=0, all bins fill toward the bootstrap target
        assert_eq!(routing.config.limits.target(b(0), d(0)), 18);
        assert_eq!(routing.config.limits.target(b(7), d(0)), 18);

        // At depth 8, targets should vary by bin
        // Bin 7: 160 × 8 / 36 = 35
        assert_eq!(routing.config.limits.target(b(7), d(8)), 35);
        // Bin 0: taper gives 160 × 1 / 36 = 4, floored at saturation (8)
        assert_eq!(routing.config.limits.target(b(0), d(8)), 8);
        // Neighborhood (bin >= depth) returns MAX
        assert_eq!(routing.config.limits.target(b(8), d(8)), usize::MAX);
    }

    #[test]
    fn test_eviction_candidates_no_surplus() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default().with_nominal(3);
        let (routing, _pm) = make_routing(base, config);

        // No peers, no surplus
        let candidates = routing.eviction_candidates(|_| 1);
        assert!(candidates.is_empty());

        // Add peers below nominal - still no surplus
        let peer1 = SwarmAddress::with_first_byte(0x80); // bin=0
        SwarmRouting::connected(&*routing, peer1);
        let candidates = routing.eviction_candidates(|_| 1);
        assert!(candidates.is_empty());
    }

    /// Helper to directly place a peer as Active in routing state (bypasses capacity checks).
    fn force_active(routing: &KademliaRouting<MockIdentity>, peer: OverlayAddress) {
        let bin = routing.bin_for(&peer);
        atomic_inc(&routing.active_counts, bin);
        routing
            .connection_phases
            .write()
            .insert(peer, ConnectionPhase::Active);
        let _ = routing.connected_peers.add(peer);
    }

    /// Helper to directly place a peer as Handshaking in routing state.
    fn force_handshaking(routing: &KademliaRouting<MockIdentity>, peer: OverlayAddress) {
        let bin = routing.bin_for(&peer);
        atomic_inc(&routing.handshaking_counts, bin);
        routing
            .connection_phases
            .write()
            .insert(peer, ConnectionPhase::Handshaking);
    }

    #[test]
    fn test_eviction_candidates_handshaking_first() {
        let base = SwarmAddress::with_first_byte(0x00);
        // Pin the trim floor (oversaturation_peers) and saturation to 4 so
        // a small bin-0 population yields a surplus and the depth-8 bin-0
        // target stays at 4 as the original scenario assumed.
        let config = KademliaConfig::default()
            .with_bootstrap_target(4)
            .with_oversaturation_peers(4)
            .with_saturation(4);
        let (routing, _pm) = make_routing(base, config);

        // Place 5 active peers in bin 0 (bin=0)
        let active_peers: Vec<_> = (0..5)
            .map(|i| {
                let mut bytes = [0x00u8; 32];
                bytes[0] = 0x80 + i;
                OverlayAddress::from(bytes)
            })
            .collect();
        for &peer in &active_peers {
            force_active(&routing, peer);
        }

        // Place 1 handshaking peer in bin 0
        let handshaking = {
            let mut bytes = [0x00u8; 32];
            bytes[0] = 0x90;
            OverlayAddress::from(bytes)
        };
        force_handshaking(&routing, handshaking);

        // Simulate depth increase to 8 (bin 0 target = max(160*1/36, 3) = 4)
        // effective = 6 (5 active + 1 handshaking), surplus = 2
        routing.depth.store(8, Ordering::Relaxed);

        let candidates = routing.eviction_candidates(|_| 1);
        assert_eq!(candidates.len(), 2);
        // Handshaking peer should be selected
        assert!(
            candidates
                .iter()
                .any(|c| c.overlay == handshaking && c.phase == EvictionPhase::Handshaking)
        );
        // One active peer should also be selected
        assert!(candidates.iter().any(|c| c.phase == EvictionPhase::Active));
    }

    #[test]
    fn test_eviction_candidates_active_lowest_score() {
        let base = SwarmAddress::with_first_byte(0x00);
        // Trim floor pinned to 4; see test_eviction_candidates_handshaking_first.
        let config = KademliaConfig::default()
            .with_bootstrap_target(4)
            .with_oversaturation_peers(4)
            .with_saturation(4);
        let (routing, _pm) = make_routing(base, config);

        // Place 6 active peers in bin 0
        let peers: Vec<_> = (0..6)
            .map(|i| {
                let mut bytes = [0x00u8; 32];
                bytes[0] = 0x80 + i;
                OverlayAddress::from(bytes)
            })
            .collect();
        for &peer in &peers {
            force_active(&routing, peer);
        }

        // Simulate depth increase to 8 (bin 0 target = 4, surplus = 2)
        routing.depth.store(8, Ordering::Relaxed);

        let candidates = routing.eviction_candidates(|_| 1);
        assert_eq!(candidates.len(), 2);
        for c in &candidates {
            assert_eq!(c.phase, EvictionPhase::Active);
            assert_eq!(c.bin.get(), 0);
        }
    }

    #[test]
    fn test_eviction_ignores_in_flight_dials() {
        let base = SwarmAddress::with_first_byte(0x00);
        // Trim floor pinned to 4; see test_eviction_candidates_handshaking_first.
        let config = KademliaConfig::default()
            .with_bootstrap_target(4)
            .with_oversaturation_peers(4)
            .with_saturation(4);
        let (routing, _pm) = make_routing(base, config);

        // 5 active peers and 3 in-flight dials in bin 0. At depth 8 the trim
        // floor is 4: the evictable population (5 active) carries a surplus
        // of 1. Counting the dials in would claim a surplus of 4 and cut the
        // bin's active set to 1, far below saturation, flapping depth.
        for i in 0..5 {
            let mut bytes = [0x00u8; 32];
            bytes[0] = 0x80 + i;
            force_active(&routing, OverlayAddress::from(bytes));
        }
        for i in 0..3 {
            let mut bytes = [0x00u8; 32];
            bytes[0] = 0x90 + i;
            let peer = OverlayAddress::from(bytes);
            atomic_inc(&routing.dialing_counts, routing.bin_for(&peer));
            routing
                .connection_phases
                .write()
                .insert(peer, ConnectionPhase::Dialing);
        }
        routing.depth.store(8, Ordering::Relaxed);

        let candidates = routing.eviction_candidates(|_| 1);
        assert_eq!(
            candidates.len(),
            1,
            "surplus must be computed from the evictable population only"
        );
        assert_eq!(candidates[0].phase, EvictionPhase::Active);
    }

    #[test]
    fn test_eviction_prefers_least_reachable() {
        let base = SwarmAddress::with_first_byte(0x00);
        // Trim floor pinned to 4; see test_eviction_candidates_handshaking_first.
        let (routing, _pm) = make_routing(
            base,
            KademliaConfig::default()
                .with_bootstrap_target(4)
                .with_oversaturation_peers(4)
                .with_saturation(4),
        );

        // 6 active peers in bin 0; at depth 8 the target is 4, so surplus is 2.
        let peers: Vec<_> = (0..6)
            .map(|i| {
                let mut bytes = [0x00u8; 32];
                bytes[0] = 0x80 + i;
                OverlayAddress::from(bytes)
            })
            .collect();
        for &peer in &peers {
            force_active(&routing, peer);
        }
        routing.depth.store(8, Ordering::Relaxed);

        // Two peers are least-reachable (rank 0); the rest are Public (rank 2).
        // With equal scores, reachability decides: the two unreachable peers
        // must be the eviction victims regardless of position.
        let unreachable = [peers[1], peers[4]];
        let candidates = routing
            .eviction_candidates(|overlay| if unreachable.contains(overlay) { 0 } else { 2 });

        assert_eq!(candidates.len(), 2);
        let evicted: Vec<_> = candidates.iter().map(|c| c.overlay).collect();
        for peer in unreachable {
            assert!(
                evicted.contains(&peer),
                "least-reachable peer should be evicted before reachable ones"
            );
        }
    }

    #[test]
    fn test_eviction_local_tiebreak_keeps_local_over_equal_remote() {
        use crate::PeerReachability;

        let base = SwarmAddress::with_first_byte(0x00);
        // Trim floor pinned to 4; see test_eviction_candidates_handshaking_first.
        let (routing, _pm) = make_routing(
            base,
            KademliaConfig::default()
                .with_bootstrap_target(4)
                .with_oversaturation_peers(4)
                .with_saturation(4),
        );

        // 6 active peers in bin 0; at depth 8 the target is 4, so surplus is 2.
        let peers: Vec<_> = (0..6)
            .map(|i| {
                let mut bytes = [0x00u8; 32];
                bytes[0] = 0x80 + i;
                OverlayAddress::from(bytes)
            })
            .collect();
        for &peer in &peers {
            force_active(&routing, peer);
        }
        routing.depth.store(8, Ordering::Relaxed);

        // Every peer has the same Unknown reachability; two are local. With the
        // `(reachability, is_local)` tuple, `false < true`, so the two non-local
        // peers rank lowest and are evicted while the locals are kept.
        let local = [peers[1], peers[4]];
        let candidates = routing
            .eviction_candidates(|overlay| (PeerReachability::Unknown, local.contains(overlay)));

        assert_eq!(candidates.len(), 2);
        let evicted: Vec<_> = candidates.iter().map(|c| c.overlay).collect();
        for peer in local {
            assert!(
                !evicted.contains(&peer),
                "local peer must outrank a remote of equal reachability"
            );
        }
    }

    #[test]
    fn test_eviction_remote_reachable_outranks_local_unreachable() {
        use crate::PeerReachability;

        let base = SwarmAddress::with_first_byte(0x00);
        // Trim floor pinned to 4; see test_eviction_candidates_handshaking_first.
        let (routing, _pm) = make_routing(
            base,
            KademliaConfig::default()
                .with_bootstrap_target(4)
                .with_oversaturation_peers(4)
                .with_saturation(4),
        );

        // 5 active peers in bin 0; at depth 8 the target is 4, so surplus is 1.
        let peers: Vec<_> = (0..5)
            .map(|i| {
                let mut bytes = [0x00u8; 32];
                bytes[0] = 0x80 + i;
                OverlayAddress::from(bytes)
            })
            .collect();
        for &peer in &peers {
            force_active(&routing, peer);
        }
        routing.depth.store(8, Ordering::Relaxed);

        // One local-but-unreachable peer; the rest are remote and reachable.
        // Locality is the low-order tiebreak, so a genuine liveness failure
        // (Unreachable) still ranks the local peer lowest and evicts it.
        let dead_local = peers[2];
        let candidates = routing.eviction_candidates(|overlay| {
            if *overlay == dead_local {
                (PeerReachability::Unreachable, true)
            } else {
                (PeerReachability::Reachable, false)
            }
        });

        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].overlay, dead_local,
            "an unreachable local is still evicted when it is the worst candidate"
        );
    }

    #[test]
    fn test_select_trim_victims_prefix_diversity_within_ties() {
        // Two sub-tries inside one bin: a tight cluster sharing a long prefix
        // (first bytes 0x80..0x83, mutual proximity >= 6) and a spread pair
        // (0xC0 and 0xE0, mutual proximity 2). With every (rank, score) equal,
        // both victims must come from the tight cluster so the kept set still
        // covers both sub-tries.
        let cluster: Vec<OverlayAddress> =
            (0x80..=0x83u8).map(SwarmAddress::with_first_byte).collect();
        let spread = [
            SwarmAddress::with_first_byte(0xc0),
            SwarmAddress::with_first_byte(0xe0),
        ];

        let pool: Vec<(OverlayAddress, u8, f64)> = cluster
            .iter()
            .chain(spread.iter())
            .map(|overlay| (*overlay, 1u8, 0.0))
            .collect();

        let victims = select_trim_victims(pool, 2);
        assert_eq!(victims.len(), 2);
        for victim in &victims {
            assert!(
                cluster.contains(victim),
                "victim {victim} should come from the clustered sub-trie"
            );
        }
    }

    #[test]
    fn test_select_trim_victims_rank_and_score_dominate() {
        // The diversity tie-break never overrides the primary order: a
        // prefix-diverse peer with the worst rank (or, at equal rank, the
        // lowest score) is still evicted first.
        let clustered_a = SwarmAddress::with_first_byte(0x80);
        let clustered_b = SwarmAddress::with_first_byte(0x81);
        let diverse = SwarmAddress::with_first_byte(0xc0);

        // Worst rank loses despite being the diversity-preferred keep.
        let pool = vec![
            (clustered_a, 2u8, 0.0),
            (clustered_b, 2u8, 0.0),
            (diverse, 0u8, 0.0),
        ];
        assert_eq!(select_trim_victims(pool, 1), vec![diverse]);

        // Equal rank: lowest score loses despite diversity.
        let pool = vec![
            (clustered_a, 1u8, 0.0),
            (clustered_b, 1u8, 0.0),
            (diverse, 1u8, -1.0),
        ];
        assert_eq!(select_trim_victims(pool, 1), vec![diverse]);
    }

    #[test]
    fn test_select_trim_victims_spreads_across_equal_clusters() {
        // Two equally tight clusters and one victim slot per round: the
        // incremental recompute alternates clusters instead of draining one,
        // keeping the survivors spread.
        let cluster_a: Vec<OverlayAddress> =
            (0x80..=0x81u8).map(SwarmAddress::with_first_byte).collect();
        let cluster_b: Vec<OverlayAddress> =
            (0xc0..=0xc1u8).map(SwarmAddress::with_first_byte).collect();

        let pool: Vec<(OverlayAddress, u8, f64)> = cluster_a
            .iter()
            .chain(cluster_b.iter())
            .map(|overlay| (*overlay, 1u8, 0.0))
            .collect();

        let victims = select_trim_victims(pool, 2);
        assert_eq!(victims.len(), 2);
        assert_eq!(
            victims.iter().filter(|v| cluster_a.contains(v)).count(),
            1,
            "one victim from each cluster keeps both sub-tries represented"
        );
    }

    #[test]
    fn test_eviction_tiebreak_keeps_bin_prefix_spread() {
        let base = SwarmAddress::with_first_byte(0x00);
        // Trim floor pinned to 4; see test_eviction_candidates_handshaking_first.
        let (routing, _pm) = make_routing(
            base,
            KademliaConfig::default()
                .with_bootstrap_target(4)
                .with_oversaturation_peers(4)
                .with_saturation(4),
        );

        // Bin 0 holds two sub-tries: four clustered peers (0x80..0x83) and two
        // spread peers (0xC0, 0xE0). At depth 8 the target is 4, so surplus is
        // 2. All ranks and scores are equal, so prefix diversity decides: both
        // victims come from the cluster and the kept set covers both sub-tries.
        let cluster: Vec<OverlayAddress> =
            (0x80..=0x83u8).map(SwarmAddress::with_first_byte).collect();
        let spread = [
            SwarmAddress::with_first_byte(0xc0),
            SwarmAddress::with_first_byte(0xe0),
        ];
        for peer in cluster.iter().chain(spread.iter()) {
            force_active(&routing, *peer);
        }
        routing.depth.store(8, Ordering::Relaxed);

        let candidates = routing.eviction_candidates(|_| 1);
        assert_eq!(candidates.len(), 2);
        for c in &candidates {
            assert_eq!(c.phase, EvictionPhase::Active);
            assert!(
                cluster.contains(&c.overlay),
                "evicted {} from the spread sub-trie; the kept set must stay \
                 spread across the bin's sub-tries",
                c.overlay
            );
        }
    }

    #[test]
    fn test_eviction_candidates_neighborhood_never_evicted() {
        let base = SwarmAddress::with_first_byte(0x00);
        // Trim floor pinned to the saturation default (8); dialing alone can
        // never exceed it, so the overflow that yields eviction candidates
        // must arrive through the inbound headroom band.
        let config = KademliaConfig::default()
            .with_total_target(8)
            .with_bootstrap_target(8)
            .with_oversaturation_peers(8);
        let (routing, _pm) = make_routing(base, config);

        let connect = |peer: OverlayAddress| {
            routing.try_reserve_dial(&peer, SwarmNodeType::Storer);
            routing.dial_connected(&peer);
            routing.handshake_completed(&peer);
            SwarmRouting::connected(&*routing, peer);
        };
        let accept_inbound = |peer: OverlayAddress| {
            routing.reserve_inbound(&peer);
            routing.handshake_completed(&peer);
            SwarmRouting::connected(&*routing, peer);
        };

        // Fill bins 0 and 1 to the trim floor by dialing, then overfill them
        // through the inbound band; anchor bin 2 with the low watermark so
        // depth climbs to 2 and bin 2 becomes a neighborhood bin.
        for bin in 0..2 {
            for idx in 0..8 {
                connect(addr_in_bin(bin, idx));
            }
            for idx in 8..10 {
                accept_inbound(addr_in_bin(bin, idx));
            }
        }
        for idx in 0..6 {
            connect(addr_in_bin(2, idx));
        }

        assert_eq!(routing.depth().get(), 2);

        let candidates = routing.eviction_candidates(|_| 1);
        // Below-depth bins (0, 1) overflow their small target; the neighborhood
        // bin (2) never produces candidates regardless of its population.
        assert!(!candidates.is_empty());
        for c in &candidates {
            assert!(
                !routing.depth().contains(c.bin),
                "neighborhood bin {} should not produce candidates",
                c.bin
            );
        }
    }
}
