//! Typed saturation decisions and bin-admission policies.
//!
//! Replaces bool-returning saturation checks with explicit
//! [`SaturationDecision`] variants so the routing layer (and the
//! handshake admission gate) can express the difference between an
//! outright rejection, a clean accept, and a slot-reuse path where the
//! newcomer takes an existing peer's slot.
//!
//! Two [`BinPolicy`] implementations ship here:
//!
//! * [`StandardPolicy`] is the default; it rejects when a bin is
//!   over-saturated and accepts otherwise.
//! * [`BootnodePolicy`] is the bootnode mode; on over-saturation it
//!   nominates the lowest-ranked peer in the bin as an
//!   [`EvictionCandidate`] so the routing layer can recycle that slot
//!   for the newcomer. Eviction is the way a public bootnode stays
//!   reachable to new joiners under sustained load.
//!
//! Eviction ordering is `(reachability rank ascending, score ascending,
//! last_seen ascending, peer_id ascending)`: the dominant axis is
//! reachability so a confirmed-public peer is never evicted in favour
//! of an unknown one regardless of score.

use std::cmp::Ordering;

use libp2p::PeerId;

use vertex_swarm_primitives::OverlayAddress;

use crate::reachability::PeerReachability;

/// Look up the reachability status of a peer.
///
/// Implementations are free to consult AutoNAT, the per-peer
/// reachability tracker, or any other source. The default
/// [`UnknownReachability`] reports every peer as
/// [`PeerReachability::Unknown`], useful for tests that do not exercise
/// the reachability axis.
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
    /// Bin is at or above its depth-aware ceiling and the policy refuses
    /// to evict.
    BinFull,
    /// Neighborhood bin is full. Unreachable today because neighborhood
    /// bins are unbounded; reserved so a future neighborhood cap remains
    /// expressible without API churn.
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
/// can take its slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvictionCandidate {
    /// Stable libp2p peer identifier of the evictee.
    pub peer_id: PeerId,
    /// Overlay address of the evictee.
    pub overlay: OverlayAddress,
    /// Reason this candidate was selected.
    pub reason: EvictReason,
}

/// Outcome of a [`BinPolicy::decide`] call.
///
/// Replaces the bool returns previously used by the saturation check.
/// `#[non_exhaustive]` so a future "defer-and-retry" variant remains
/// additive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SaturationDecision {
    /// A slot is available; admit the peer without eviction.
    Accept,
    /// The bin is full but the policy chose to recycle a slot; admit
    /// the peer after evicting the nominated [`EvictionCandidate`].
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
    /// Current peer score. Lower is worse.
    pub score: f64,
    /// Unix-seconds timestamp of the last successful observation. Older
    /// is worse.
    pub last_seen: u64,
}

/// Snapshot the policy needs to make a decision for a single bin.
///
/// Borrowed for the lifetime of the call so the policy never owns or
/// retains routing state. Marked `#[non_exhaustive]` so additional
/// signals (e.g. reachability of the incoming peer) can be added
/// without breaking implementers.
#[non_exhaustive]
pub struct SaturationContext<'a> {
    /// Target bin index for the incoming peer.
    pub bin: u8,
    /// `true` when the bin is at or above its inbound ceiling. The
    /// caller computes this against its own depth-aware limits so the
    /// policy never reaches into the routing layer.
    pub oversaturated: bool,
    /// Per-peer stats for the bin, in arbitrary order. The policy ranks
    /// these to pick an eviction candidate.
    pub bin_peers: &'a [PeerStats],
    /// Reachability lookup; used as the dominant eviction sort key.
    pub reachability: &'a dyn ReachabilityProvider,
}

impl<'a> SaturationContext<'a> {
    /// Build a context from its parts.
    pub fn new(
        bin: u8,
        oversaturated: bool,
        bin_peers: &'a [PeerStats],
        reachability: &'a dyn ReachabilityProvider,
    ) -> Self {
        Self {
            bin,
            oversaturated,
            bin_peers,
            reachability,
        }
    }

    /// Pick the worst peer in `bin_peers` according to the eviction
    /// ordering: ascending `(reachability rank, score, last_seen,
    /// peer_id)`.
    ///
    /// Returns `None` when the bin is empty (e.g. when oversaturation
    /// is driven entirely by in-progress dial or handshake slots).
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
/// The trailing `peer_id` byte comparison breaks any remaining ties so
/// identical-looking peers always rank in a stable, hash-independent
/// order.
fn compare_for_eviction(
    reachability: &dyn ReachabilityProvider,
    a: &PeerStats,
    b: &PeerStats,
) -> Ordering {
    let ra = reachability.reachability(&a.peer_id).rank();
    let rb = reachability.reachability(&b.peer_id).rank();
    ra.cmp(&rb)
        .then_with(|| a.score.partial_cmp(&b.score).unwrap_or(Ordering::Equal))
        .then(a.last_seen.cmp(&b.last_seen))
        .then(a.peer_id.to_bytes().cmp(&b.peer_id.to_bytes()))
}

/// Policy that maps a [`SaturationContext`] to a [`SaturationDecision`].
///
/// Implementations are selected by the topology builder based on
/// [`vertex_swarm_primitives::SwarmNodeType`]: bootnodes use
/// [`BootnodePolicy`], every other role uses [`StandardPolicy`].
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
        if ctx.oversaturated {
            SaturationDecision::Reject(RejectionReason::BinFull)
        } else {
            SaturationDecision::Accept
        }
    }
}

/// Bootnode policy: when the target bin is over-saturated, evict the
/// lowest-ranked peer in the bin instead of rejecting.
///
/// The eviction path lets a public bootnode keep accepting new joiners
/// under sustained load instead of becoming unreachable once its bins
/// fill.
#[derive(Debug, Clone, Copy, Default)]
pub struct BootnodePolicy;

impl BinPolicy for BootnodePolicy {
    fn decide(&self, ctx: &SaturationContext<'_>) -> SaturationDecision {
        if !ctx.oversaturated {
            return SaturationDecision::Accept;
        }
        match ctx.worst_peer() {
            Some(c) => SaturationDecision::AcceptEvict(c),
            None => {
                // The bin reports over-saturation but no peers were
                // supplied for eviction. The oversaturation came from
                // in-progress (dialing or handshaking) slots that the
                // caller did not enumerate. Fall back to BinFull so the
                // routing layer does not admit a peer it cannot make
                // room for.
                SaturationDecision::Reject(RejectionReason::BinFull)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use vertex_swarm_test_utils::{make_overlay, test_peer_id};

    use super::*;

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
        let reach = UnknownReachability;
        let peers: Vec<PeerStats> = Vec::new();
        let ctx = SaturationContext::new(0, false, &peers, &reach);
        assert_eq!(StandardPolicy.decide(&ctx), SaturationDecision::Accept);
    }

    #[test]
    fn standard_policy_rejects_when_over_ceiling() {
        let reach = UnknownReachability;
        let peers: Vec<PeerStats> = (0..9).map(|i| peer_stats(0x80 + i, 0.0, 0)).collect();
        let ctx = SaturationContext::new(0, true, &peers, &reach);
        assert_eq!(
            StandardPolicy.decide(&ctx),
            SaturationDecision::Reject(RejectionReason::BinFull),
        );
    }

    #[test]
    fn bootnode_policy_accepts_when_under_ceiling() {
        let reach = UnknownReachability;
        let peers: Vec<PeerStats> = Vec::new();
        let ctx = SaturationContext::new(0, false, &peers, &reach);
        assert_eq!(BootnodePolicy.decide(&ctx), SaturationDecision::Accept);
    }

    #[test]
    fn bootnode_policy_evicts_lowest_score_when_over() {
        let reach = UnknownReachability;

        let worst = peer_stats(0x81, -50.0, 100);
        let middle = peer_stats(0x82, 0.0, 100);
        let best = peer_stats(0x83, 50.0, 100);
        let peers = vec![best, middle, worst];

        let ctx = SaturationContext::new(0, true, &peers, &reach);
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
        let reach = UnknownReachability;
        let peers: Vec<PeerStats> = Vec::new();
        let ctx = SaturationContext::new(0, true, &peers, &reach);
        assert_eq!(
            BootnodePolicy.decide(&ctx),
            SaturationDecision::Reject(RejectionReason::BinFull),
        );
    }

    #[test]
    fn eviction_tiebreak_uses_last_seen_then_peer_id() {
        let reach = UnknownReachability;

        let p_old = peer_stats(0x81, 0.0, 100);
        let p_new = peer_stats(0x82, 0.0, 200);
        let peers = vec![p_new, p_old];

        let ctx = SaturationContext::new(0, true, &peers, &reach);
        match BootnodePolicy.decide(&ctx) {
            SaturationDecision::AcceptEvict(c) => assert_eq!(c.overlay, p_old.overlay),
            other => panic!("expected AcceptEvict, got {other:?}"),
        }
    }

    #[test]
    fn reachability_dominates_score() {
        // Highest-score peer is Public; lowest is Private. Policy must
        // evict the Private one despite the score gap because Private
        // ranks below Public.
        let good_score_public = peer_stats(0x81, 75.0, 200);
        let low_score_private = peer_stats(0x82, -10.0, 200);
        let peers = vec![good_score_public, low_score_private];
        let reach = StubReachability::new()
            .with(good_score_public.peer_id, PeerReachability::Public)
            .with(low_score_private.peer_id, PeerReachability::Private);

        let ctx = SaturationContext::new(0, true, &peers, &reach);
        match BootnodePolicy.decide(&ctx) {
            SaturationDecision::AcceptEvict(c) => {
                assert_eq!(c.overlay, low_score_private.overlay);
            }
            other => panic!("expected AcceptEvict, got {other:?}"),
        }
    }

    #[test]
    fn private_evicted_before_unknown() {
        // Three-rank check: Private < Unknown < Public. A bin with one
        // of each ranks Private as the eviction target.
        let private = peer_stats(0x81, 50.0, 100);
        let unknown = peer_stats(0x82, 0.0, 100);
        let public = peer_stats(0x83, -50.0, 100);
        let peers = vec![public, unknown, private];

        let reach = StubReachability::new()
            .with(private.peer_id, PeerReachability::Private)
            .with(public.peer_id, PeerReachability::Public);

        let ctx = SaturationContext::new(0, true, &peers, &reach);
        match BootnodePolicy.decide(&ctx) {
            SaturationDecision::AcceptEvict(c) => assert_eq!(c.overlay, private.overlay),
            other => panic!("expected AcceptEvict, got {other:?}"),
        }
    }

    #[test]
    fn both_policies_accept_when_not_oversaturated() {
        let reach = UnknownReachability;
        let peers: Vec<PeerStats> = (0..3).map(|i| peer_stats(0x80 + i, 0.0, 100)).collect();
        let ctx = SaturationContext::new(0, false, &peers, &reach);
        assert_eq!(StandardPolicy.decide(&ctx), SaturationDecision::Accept);
        assert_eq!(BootnodePolicy.decide(&ctx), SaturationDecision::Accept);
    }
}
