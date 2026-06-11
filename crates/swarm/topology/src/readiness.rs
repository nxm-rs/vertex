//! Deterministic node-readiness state assembled from authoritative
//! topology sources.
//!
//! [`ReadinessSnapshot`] is built by `TopologyHandle::readiness` from the
//! Kademlia routing table (connected peers per bin, neighborhood depth,
//! depth-aware bin targets) and the peer manager (handshake-confirmed node
//! types). The predicate methods on the snapshot define the readiness
//! conditions that `TopologyHandle::wait_until` and its named shorthands
//! await: routability, target depth, neighborhood saturation, the
//! composite warm signal selected by the local node type, and the
//! time-stable neighborhood readiness that gates pull-syncing.

use std::time::Duration;

use vertex_swarm_primitives::{Bin, NeighborhoodDepth, SwarmNodeType};

use crate::kademlia::TopologyPhase;

/// Connection shortfall for one bin relative to its depth-aware target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinReadiness {
    /// The bin.
    pub bin: Bin,
    /// Connected peers currently in the bin.
    pub connected: usize,
    /// The bin's connection target from the depth-aware limits, or `None`
    /// for neighborhood bins, which connect to every available peer.
    pub target: Option<usize>,
    /// Peers still missing before the bin reaches its target. Zero for
    /// neighborhood bins and for bins at or above target.
    pub deficit: usize,
}

/// A cheap, consistent-enough snapshot of the node's readiness state.
///
/// Counts come from the routing table's connected-peer index and the peer
/// manager's handshake-confirmed node types, not from metrics. Fields are
/// read individually without a global lock, so a snapshot taken during
/// heavy churn can be transiently inconsistent between fields; every field
/// is exact the moment it is read.
#[derive(Debug, Clone)]
pub struct ReadinessSnapshot {
    /// The local node's type, selecting the composite warm condition
    /// (see [`Self::is_warm`]).
    pub local_node_type: SwarmNodeType,
    /// Total connected peers in the routing table.
    pub connected_peers: usize,
    /// Connected peers whose handshake-confirmed node type stores chunks.
    pub connected_storers: usize,
    /// Current neighborhood depth.
    pub depth: NeighborhoodDepth,
    /// Connected peers in bins at or beyond the depth boundary.
    pub neighborhood_connected: usize,
    /// Per-bin saturation target from the network spec; the threshold the
    /// neighborhood must reach for [`Self::is_saturated`].
    pub saturation_threshold: usize,
    /// Per-bin readiness, shallowest bin first.
    pub bins: Vec<BinReadiness>,
    /// Bins with a finite target whose connected count meets it.
    pub bins_at_target: usize,
    /// How long the neighborhood has been continuously saturated at an
    /// unchanged depth as of this snapshot, or `None` while it is below
    /// saturation. Tracked by the routing table on every mutation, so a
    /// dip between two snapshots restarts the clock even if no snapshot
    /// observed it. The clock's saturation threshold is the one the depth
    /// frontier derives from, which on production construction paths is
    /// the same spec value as [`Self::saturation_threshold`].
    pub neighborhood_stable_for: Option<Duration>,
    /// The configured window [`Self::neighborhood_stable_for`] must reach
    /// for [`Self::is_neighborhood_ready`].
    pub neighborhood_stability_window: Duration,
    /// Current topology phase, as last committed by the phase machine
    /// (re-derived on every connect, disconnect, and periodic evaluation
    /// tick, so it can lag a quiet table by at most one tick).
    pub phase: TopologyPhase,
    /// Time spent in the current phase.
    pub time_in_phase: Duration,
}

impl ReadinessSnapshot {
    /// Whether a chunk push or retrieval has a peer to ask: at least one
    /// connected storer.
    #[must_use]
    pub fn is_routable(&self) -> bool {
        self.connected_storers > 0
    }

    /// Whether the neighborhood depth has reached `min_depth`.
    #[must_use]
    pub fn depth_reached(&self, min_depth: NeighborhoodDepth) -> bool {
        self.depth >= min_depth
    }

    /// Whether the neighborhood is saturated: a depth boundary is
    /// established and the bins at or beyond it together hold at least
    /// [`Self::saturation_threshold`] connected peers.
    ///
    /// The threshold is the same per-bin saturation target that gates the
    /// depth frontier, applied to the neighborhood as a whole.
    #[must_use]
    pub fn is_saturated(&self) -> bool {
        self.depth > NeighborhoodDepth::ZERO
            && self.neighborhood_connected >= self.saturation_threshold
    }

    /// Whether the neighborhood is ready for pull-syncing: continuously
    /// saturated at an unchanged depth for at least
    /// [`Self::neighborhood_stability_window`].
    ///
    /// Storage radius is a distinct value from connection depth, so
    /// chunk synchronization must not start on total connectivity alone:
    /// it needs the bins at and above the depth boundary to be both
    /// saturated and settled, or the responsibility boundary it syncs
    /// against is still moving. Any depth change or saturation dip
    /// restarts the clock. Stricter than [`Self::is_saturated`], which is
    /// instantaneous.
    #[must_use]
    pub fn is_neighborhood_ready(&self) -> bool {
        self.neighborhood_stable_for
            .is_some_and(|stable| stable >= self.neighborhood_stability_window)
    }

    /// The composite warm signal for the local node type.
    ///
    /// A storer is warm once it is routable and its neighborhood is
    /// saturated; it needs the neighborhood to hold the chunks it is
    /// responsible for. Every other node type is warm as soon as it is
    /// routable, the point from which pushes and retrievals can proceed.
    #[must_use]
    pub fn is_warm(&self) -> bool {
        if self.local_node_type.requires_storage() {
            self.is_routable() && self.is_saturated()
        } else {
            self.is_routable()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn depth(n: u8) -> NeighborhoodDepth {
        NeighborhoodDepth::new(Bin::new(n).expect("valid bin"))
    }

    fn snapshot(node_type: SwarmNodeType) -> ReadinessSnapshot {
        ReadinessSnapshot {
            local_node_type: node_type,
            connected_peers: 0,
            connected_storers: 0,
            depth: NeighborhoodDepth::ZERO,
            neighborhood_connected: 0,
            saturation_threshold: 8,
            bins: Vec::new(),
            bins_at_target: 0,
            neighborhood_stable_for: None,
            neighborhood_stability_window: Duration::from_secs(30),
            phase: TopologyPhase::Bootstrap,
            time_in_phase: Duration::ZERO,
        }
    }

    #[test]
    fn empty_snapshot_is_cold() {
        let s = snapshot(SwarmNodeType::Client);
        assert!(!s.is_routable());
        assert!(!s.is_saturated());
        assert!(!s.is_warm());
        assert!(s.depth_reached(NeighborhoodDepth::ZERO));
        assert!(!s.depth_reached(depth(1)));
    }

    #[test]
    fn routable_with_one_storer() {
        let mut s = snapshot(SwarmNodeType::Client);
        s.connected_peers = 1;
        s.connected_storers = 1;
        assert!(s.is_routable());
        assert!(s.is_warm(), "a client is warm as soon as it can route");
        assert!(!s.is_saturated());
    }

    #[test]
    fn clients_alone_are_not_routable() {
        let mut s = snapshot(SwarmNodeType::Client);
        s.connected_peers = 5;
        s.connected_storers = 0;
        assert!(!s.is_routable());
        assert!(!s.is_warm());
    }

    #[test]
    fn saturation_requires_established_depth() {
        let mut s = snapshot(SwarmNodeType::Storer);
        s.connected_peers = 20;
        s.connected_storers = 20;
        s.neighborhood_connected = 20;
        // Depth 0: no boundary established yet, so not saturated.
        assert!(!s.is_saturated());
        assert!(!s.is_warm(), "a storer is not warm before saturation");

        s.depth = depth(1);
        assert!(s.is_saturated());
        assert!(s.is_warm());
    }

    #[test]
    fn saturation_requires_threshold_in_neighborhood() {
        let mut s = snapshot(SwarmNodeType::Storer);
        s.connected_peers = 30;
        s.connected_storers = 30;
        s.depth = depth(2);
        s.neighborhood_connected = 7;
        assert!(!s.is_saturated());

        s.neighborhood_connected = 8;
        assert!(s.is_saturated());
    }

    #[test]
    fn neighborhood_ready_requires_full_window() {
        let mut s = snapshot(SwarmNodeType::Storer);
        assert!(!s.is_neighborhood_ready(), "unsaturated is never ready");

        s.neighborhood_stable_for = Some(Duration::from_secs(29));
        assert!(!s.is_neighborhood_ready(), "window not yet served");

        s.neighborhood_stable_for = Some(Duration::from_secs(30));
        assert!(s.is_neighborhood_ready(), "window boundary is inclusive");

        s.neighborhood_stable_for = Some(Duration::from_secs(31));
        assert!(s.is_neighborhood_ready());
    }

    #[test]
    fn neighborhood_ready_with_zero_window_is_saturation() {
        let mut s = snapshot(SwarmNodeType::Storer);
        s.neighborhood_stability_window = Duration::ZERO;
        assert!(!s.is_neighborhood_ready(), "still needs saturation");

        s.neighborhood_stable_for = Some(Duration::ZERO);
        assert!(s.is_neighborhood_ready());
    }

    #[test]
    fn depth_reached_is_monotonic_in_min_depth() {
        let mut s = snapshot(SwarmNodeType::Client);
        s.depth = depth(3);
        assert!(s.depth_reached(depth(1)));
        assert!(s.depth_reached(depth(3)));
        assert!(!s.depth_reached(depth(4)));
    }
}
