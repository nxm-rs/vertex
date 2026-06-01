//! Typed saturation decisions and bin-admission policies.
//!
//! Replaces bool-returning saturation checks with explicit
//! [`SaturationDecision`] variants so the routing layer (and future
//! handshake admission control in Unit 5) can express the difference
//! between an outright rejection, a clean accept, and a slot-reuse
//! ("accept-evict") path.
//!
//! Two [`BinPolicy`] implementations are provided:
//!
//! - [`StandardPolicy`] — the default; mirrors today's `Reject` behaviour
//!   when a bin is over-saturated.
//! - [`BootnodePolicy`] — bootnode mode; on over-saturation it evicts the
//!   lowest-quality peer in the target bin and admits the newcomer. This
//!   mirrors bee's
//!   [`Kad.Connected`](https://github.com/ethersphere/bee/blob/master/pkg/topology/kademlia/kademlia.go#L1193-L1199)
//!   bootnode branch.
//!
//! Eviction selection orders peers ascending by
//! `(reachability, peer_score, last_seen, peer_id)` so the least-useful
//! peer is evicted first. Reachability is supplied via the
//! [`ReachabilityProvider`] trait — the default
//! [`UnknownReachability`] reports every peer as
//! [`PeerReachability::Unknown`]; Unit 10 will wire the full AutoNAT +
//! stabilization bridge.

use std::cmp::Ordering;

use libp2p::PeerId;

use vertex_swarm_primitives::OverlayAddress;

pub use super::limits::DepthAwareLimits;

/// Coarse reachability bucket for a peer, used as the dominant eviction
/// sort key.
///
/// Mirrors bee's `p2p.ReachabilityStatus` (`pkg/p2p/p2p.go`). Unit 10
/// will replace the in-module definition with the project-wide
/// `crate::reachability::PeerReachability` once that module is wired
/// into the routing layer; today this lightweight enum keeps the
/// saturation module self-contained and lets Unit 8 land before
/// Unit 10's full tracker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum PeerReachability {
    /// No reachability signal observed.
    #[default]
    Unknown,
    /// Peer is reachable from the public internet.
    Public,
}

impl PeerReachability {
    /// Eviction rank: lower is evicted first.
    ///
    /// `Unknown < Public` so when forced to evict the policy keeps
    /// confirmed-reachable peers.
    #[inline]
    #[must_use]
    pub fn evict_rank(self) -> u8 {
        match self {
            PeerReachability::Unknown => 0,
            PeerReachability::Public => 1,
        }
    }
}

/// Look up the reachability status of a peer.
///
/// Implementations are free to consult AutoNAT, stabilization detector,
/// or other sources. The default [`UnknownReachability`] reports every
/// peer as [`PeerReachability::Unknown`] — useful pre-Unit-10 and for
/// tests that don't care about reachability ordering.
pub trait ReachabilityProvider: Send + Sync {
    /// Reachability status for `peer`.
    fn reachability(&self, peer: &PeerId) -> PeerReachability;
}

/// Default provider: every peer is [`PeerReachability::Unknown`].
#[derive(Debug, Clone, Copy, Default)]
pub struct UnknownReachability;

impl ReachabilityProvider for UnknownReachability {
    #[inline]
    fn reachability(&self, _peer: &PeerId) -> PeerReachability {
        PeerReachability::Unknown
    }
}

impl<T> ReachabilityProvider for &T
where
    T: ReachabilityProvider + ?Sized,
{
    #[inline]
    fn reachability(&self, peer: &PeerId) -> PeerReachability {
        (**self).reachability(peer)
    }
}

/// Why a [`SaturationDecision::Reject`] was returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum RejectionReason {
    /// Bin is at or above its depth-aware ceiling and the policy
    /// refuses to evict.
    BinFull,
    /// Neighborhood bin (`bin >= depth`) is full. Unreachable today
    /// because neighborhood bins are unbounded; reserved so that future
    /// neighborhood caps remain expressible without API churn.
    NeighborhoodFull,
    /// Total connected-peer count is at or above the configured ceiling
    /// independent of bin distribution.
    AboveCapacity,
}

/// Why a particular peer was nominated for eviction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum EvictReason {
    /// Peer ranked lowest by the
    /// `(reachability, score, last_seen, peer_id)` ordering.
    LowestReachability,
}

/// A peer the policy nominated for eviction so an incoming connection
/// can take its place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvictionCandidate {
    /// Stable libp2p peer identifier for the evictee.
    pub peer_id: PeerId,
    /// Overlay address of the evictee.
    pub overlay: OverlayAddress,
    /// Reason this candidate was selected.
    pub reason: EvictReason,
}

/// Outcome of a [`BinPolicy::decide`] call.
///
/// Replaces the bool returns previously used by saturation checks; the
/// caller now sees the three distinct outcomes a policy can produce and
/// the compiler enforces that every branch is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SaturationDecision {
    /// Slot is available — admit the peer without eviction.
    Accept,
    /// Slot must be reclaimed — admit the peer after evicting the
    /// nominated [`EvictionCandidate`].
    AcceptEvict(EvictionCandidate),
    /// Refuse the peer; do not modify the routing table.
    Reject(RejectionReason),
}

/// Compact peer record used by the policy to rank eviction candidates.
///
/// Constructed by the caller (the routing layer) from its private peer
/// metadata so the saturation module needs no knowledge of the wider
/// peer-manager interface.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PeerStats {
    /// Stable libp2p peer identifier.
    pub peer_id: PeerId,
    /// Overlay (Kademlia) address.
    pub overlay: OverlayAddress,
    /// Current peer score (lower is worse).
    pub score: f64,
    /// Unix timestamp (seconds) of the last successful observation.
    /// Older is considered worse.
    pub last_seen: u64,
}

/// Snapshot the policy needs to make a decision for a single bin.
///
/// Borrowed for the lifetime of the call so the policy never owns or
/// retains routing state. Marked `#[non_exhaustive]` so additional
/// signals (e.g. reachability of the *incoming* peer) can be added
/// without breaking downstream implementers.
#[non_exhaustive]
pub struct SaturationContext<'a> {
    /// Target bin index for the incoming peer.
    pub bin: u8,
    /// Current Kademlia depth.
    pub depth: u8,
    /// Number of effective connections (dialing + handshaking + active)
    /// already in the bin.
    pub effective_count: usize,
    /// Depth-aware limits used to interpret `effective_count`.
    pub limits: &'a DepthAwareLimits,
    /// Per-peer stats for the bin, in arbitrary order. The policy is
    /// responsible for picking an eviction candidate from this slice.
    pub bin_peers: &'a [PeerStats],
    /// Reachability lookup, used as the dominant eviction sort key.
    pub reachability: &'a dyn ReachabilityProvider,
}

impl<'a> SaturationContext<'a> {
    /// Construct a context. Kept on the struct to make trait-object
    /// invocations terse at the call site.
    pub fn new(
        bin: u8,
        depth: u8,
        effective_count: usize,
        limits: &'a DepthAwareLimits,
        bin_peers: &'a [PeerStats],
        reachability: &'a dyn ReachabilityProvider,
    ) -> Self {
        Self {
            bin,
            depth,
            effective_count,
            limits,
            bin_peers,
            reachability,
        }
    }

    /// True if the bin is over-saturated under the current depth-aware
    /// ceiling. Mirrors `DepthAwareLimits::should_accept_inbound` but
    /// in the explicit "would be over" direction.
    #[inline]
    pub fn is_oversaturated(&self) -> bool {
        !self
            .limits
            .should_accept_inbound(self.bin, self.depth, self.effective_count)
    }

    /// True if the target bin is in the neighborhood (`bin >= depth`).
    /// Neighborhood bins are unbounded today.
    #[inline]
    pub fn is_neighborhood(&self) -> bool {
        self.depth > 0 && self.bin >= self.depth
    }

    /// Pick the worst peer in `bin_peers` according to the eviction
    /// ordering: `(reachability rank, score, last_seen, peer_id)`
    /// ascending.
    ///
    /// Returns `None` when the bin is empty.
    pub(crate) fn worst_peer(&self) -> Option<EvictionCandidate> {
        self.bin_peers
            .iter()
            .min_by(|a, b| compare_for_eviction(self.reachability, a, b))
            .map(|p| EvictionCandidate {
                peer_id: p.peer_id,
                overlay: p.overlay,
                reason: EvictReason::LowestReachability,
            })
    }
}

/// Deterministic ordering for eviction: ascending by
/// `(reachability rank, score, last_seen, peer_id)`.
///
/// The `peer_id` tail breaks remaining ties deterministically so tests
/// over identical-looking peers always observe a stable answer rather
/// than one dependent on hash iteration order.
fn compare_for_eviction(
    reachability: &dyn ReachabilityProvider,
    a: &PeerStats,
    b: &PeerStats,
) -> Ordering {
    let ra = reachability.reachability(&a.peer_id).evict_rank();
    let rb = reachability.reachability(&b.peer_id).evict_rank();
    ra.cmp(&rb)
        .then_with(|| a.score.partial_cmp(&b.score).unwrap_or(Ordering::Equal))
        .then(a.last_seen.cmp(&b.last_seen))
        .then(a.peer_id.to_bytes().cmp(&b.peer_id.to_bytes()))
}

/// Policy that maps a [`SaturationContext`] to a [`SaturationDecision`].
///
/// Implementations are selected by the topology builder based on
/// [`vertex_swarm_primitives::SwarmNodeType`] — bootnodes use
/// [`BootnodePolicy`], everyone else uses [`StandardPolicy`].
pub trait BinPolicy: Send + Sync + 'static {
    /// Decide whether to admit a peer whose target bin is described by
    /// `ctx`.
    fn decide(&self, ctx: &SaturationContext<'_>) -> SaturationDecision;
}

/// Default policy: reject on over-saturation, accept otherwise.
#[derive(Debug, Clone, Copy, Default)]
pub struct StandardPolicy;

impl BinPolicy for StandardPolicy {
    fn decide(&self, ctx: &SaturationContext<'_>) -> SaturationDecision {
        if ctx.is_oversaturated() {
            SaturationDecision::Reject(RejectionReason::BinFull)
        } else {
            SaturationDecision::Accept
        }
    }
}

/// Bootnode policy: when the target bin is over-saturated, evict the
/// lowest-ranked peer in the bin instead of rejecting.
///
/// Mirrors bee `pkg/topology/kademlia/kademlia.go:1193-1199`.
#[derive(Debug, Clone, Copy, Default)]
pub struct BootnodePolicy;

impl BinPolicy for BootnodePolicy {
    fn decide(&self, ctx: &SaturationContext<'_>) -> SaturationDecision {
        if !ctx.is_oversaturated() {
            return SaturationDecision::Accept;
        }
        match ctx.worst_peer() {
            Some(c) => SaturationDecision::AcceptEvict(c),
            None => {
                // Bin reports over-saturation but no peers were
                // supplied for eviction — this means the
                // over-saturation came from in-progress (dialing /
                // handshaking) slots that the caller did not enumerate.
                // Fall back to BinFull so the routing layer doesn't
                // admit a peer it cannot make room for.
                SaturationDecision::Reject(RejectionReason::BinFull)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use vertex_swarm_test_utils::{make_overlay, test_peer_id};

    fn limits_at_depth_8() -> DepthAwareLimits {
        // total_target=160, nominal=3, headroom=4
        // bin 0 target at depth 8 = max(160*1/36, 3) = 4
        // ceiling                 = 4 + 4 = 8
        DepthAwareLimits::new(160, 3)
    }

    fn peer_stats(byte: u8, score: f64, last_seen: u64) -> PeerStats {
        PeerStats {
            peer_id: test_peer_id(byte),
            overlay: make_overlay(byte),
            score,
            last_seen,
        }
    }

    /// Test provider seeded with explicit per-peer reachability.
    struct StubReachability(HashMap<PeerId, PeerReachability>);
    impl StubReachability {
        fn new() -> Self {
            Self(HashMap::new())
        }
        fn with(mut self, peer: PeerId, r: PeerReachability) -> Self {
            self.0.insert(peer, r);
            self
        }
    }
    impl ReachabilityProvider for StubReachability {
        fn reachability(&self, peer: &PeerId) -> PeerReachability {
            self.0.get(peer).copied().unwrap_or_default()
        }
    }

    #[test]
    fn standard_policy_accepts_when_under_ceiling() {
        let limits = limits_at_depth_8();
        let reach = UnknownReachability;
        let peers: Vec<PeerStats> = Vec::new();
        let ctx = SaturationContext::new(0, 8, 3, &limits, &peers, &reach);
        assert_eq!(StandardPolicy.decide(&ctx), SaturationDecision::Accept);
    }

    #[test]
    fn standard_policy_rejects_when_over_ceiling() {
        let limits = limits_at_depth_8();
        let reach = UnknownReachability;
        let peers: Vec<PeerStats> = (0..9).map(|i| peer_stats(0x80 + i, 0.0, 0)).collect();
        // bin 0 ceiling at depth 8 = 8. effective = 9 -> over.
        let ctx = SaturationContext::new(0, 8, 9, &limits, &peers, &reach);
        assert_eq!(
            StandardPolicy.decide(&ctx),
            SaturationDecision::Reject(RejectionReason::BinFull)
        );
    }

    #[test]
    fn bootnode_policy_accepts_when_under_ceiling() {
        let limits = limits_at_depth_8();
        let reach = UnknownReachability;
        let peers: Vec<PeerStats> = Vec::new();
        let ctx = SaturationContext::new(0, 8, 3, &limits, &peers, &reach);
        assert_eq!(BootnodePolicy.decide(&ctx), SaturationDecision::Accept);
    }

    #[test]
    fn bootnode_policy_evicts_lowest_score_when_over() {
        let limits = limits_at_depth_8();
        let reach = UnknownReachability;

        let worst = peer_stats(0x81, -50.0, 100);
        let middle = peer_stats(0x82, 0.0, 100);
        let best = peer_stats(0x83, 50.0, 100);
        let peers = vec![best, middle, worst];

        let ctx = SaturationContext::new(0, 8, 9, &limits, &peers, &reach);
        match BootnodePolicy.decide(&ctx) {
            SaturationDecision::AcceptEvict(c) => {
                assert_eq!(c.overlay, worst.overlay);
                assert_eq!(c.peer_id, worst.peer_id);
                assert_eq!(c.reason, EvictReason::LowestReachability);
            }
            other => panic!("expected AcceptEvict, got {other:?}"),
        }
    }

    #[test]
    fn bootnode_policy_falls_back_when_no_peers_supplied() {
        let limits = limits_at_depth_8();
        let reach = UnknownReachability;
        let peers: Vec<PeerStats> = Vec::new();
        let ctx = SaturationContext::new(0, 8, 9, &limits, &peers, &reach);
        assert_eq!(
            BootnodePolicy.decide(&ctx),
            SaturationDecision::Reject(RejectionReason::BinFull)
        );
    }

    #[test]
    fn eviction_tiebreak_uses_last_seen_then_peer_id() {
        let limits = limits_at_depth_8();
        let reach = UnknownReachability;

        let p_old = peer_stats(0x81, 0.0, 100);
        let p_new = peer_stats(0x82, 0.0, 200);
        let peers = vec![p_new, p_old];

        let ctx = SaturationContext::new(0, 8, 9, &limits, &peers, &reach);
        match BootnodePolicy.decide(&ctx) {
            SaturationDecision::AcceptEvict(c) => assert_eq!(c.overlay, p_old.overlay),
            other => panic!("expected AcceptEvict, got {other:?}"),
        }
    }

    #[test]
    fn reachability_dominates_score() {
        let limits = limits_at_depth_8();
        // Highest score peer is Public; lowest is Unknown. Policy must
        // evict the Unknown one despite the score gap, because Unknown
        // has rank 0 < Public rank 1.
        let good_score_public = peer_stats(0x81, 75.0, 200);
        let mid_score_unknown = peer_stats(0x82, 10.0, 200);
        let peers = vec![good_score_public, mid_score_unknown];
        let reach = StubReachability::new()
            .with(good_score_public.peer_id, PeerReachability::Public)
            .with(mid_score_unknown.peer_id, PeerReachability::Unknown);

        let ctx = SaturationContext::new(0, 8, 9, &limits, &peers, &reach);
        match BootnodePolicy.decide(&ctx) {
            SaturationDecision::AcceptEvict(c) => {
                assert_eq!(c.overlay, mid_score_unknown.overlay);
            }
            other => panic!("expected AcceptEvict, got {other:?}"),
        }
    }

    #[test]
    fn neighborhood_bin_never_oversaturates() {
        let limits = limits_at_depth_8();
        let reach = UnknownReachability;
        let peers: Vec<PeerStats> = Vec::new();
        // bin == depth -> neighborhood, ceiling MAX
        let ctx = SaturationContext::new(8, 8, 10_000, &limits, &peers, &reach);
        assert!(!ctx.is_oversaturated());
        assert!(ctx.is_neighborhood());
        assert_eq!(StandardPolicy.decide(&ctx), SaturationDecision::Accept);
        assert_eq!(BootnodePolicy.decide(&ctx), SaturationDecision::Accept);
    }
}
