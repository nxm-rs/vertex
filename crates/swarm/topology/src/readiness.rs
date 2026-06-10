//! Deterministic node-readiness state assembled from authoritative
//! topology sources.
//!
//! [`ReadinessSnapshot`] is built by `TopologyHandle::readiness` from the
//! Kademlia routing table (connected peers per bin, neighborhood depth,
//! depth-aware bin targets) and the peer manager (handshake-confirmed node
//! types). The predicate methods on the snapshot define the readiness
//! conditions that `TopologyHandle::wait_until` and its named shorthands
//! await: routability, target depth, neighborhood saturation, and the
//! composite warm signal selected by the local node type.

use vertex_swarm_primitives::{Bin, NeighborhoodDepth, SwarmNodeType};

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
    fn depth_reached_is_monotonic_in_min_depth() {
        let mut s = snapshot(SwarmNodeType::Client);
        s.depth = depth(3);
        assert!(s.depth_reached(depth(1)));
        assert!(s.depth_reached(depth(3)));
        assert!(!s.depth_reached(depth(4)));
    }
}
