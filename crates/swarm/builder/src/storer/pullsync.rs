//! Puller seam bridges over the live topology handle.
//!
//! The node provides the concrete pullsync control and event plumbing; these two
//! seams bridge the puller's readiness gate and neighbour selection to topology.

use std::sync::Arc;

use vertex_swarm_api::{SwarmTopologyRouting, SwarmTopologyState};
use vertex_swarm_identity::Identity;
use vertex_swarm_primitives::{Bin, neighborhood_bins};
use vertex_swarm_puller::{NeighbourSource, ReadinessGate, SyncTarget};
use vertex_swarm_topology::TopologyHandle;

/// Bridges [`ReadinessGate`] to neighbourhood readiness on the topology handle.
pub(crate) struct TopologyReadiness {
    topology: TopologyHandle<Arc<Identity>>,
}

impl TopologyReadiness {
    pub(crate) fn new(topology: TopologyHandle<Arc<Identity>>) -> Self {
        Self { topology }
    }
}

impl ReadinessGate for TopologyReadiness {
    async fn wait_ready(&self) {
        // A closed topology event channel only happens on node shutdown, when the
        // puller is being torn down anyway; resolve rather than spin.
        let _ = self.topology.wait_until_neighborhood_ready().await;
    }
}

/// Bridges [`NeighbourSource`] to the connected neighbourhood: the peers at or
/// beyond the current depth, each scoped to the neighbourhood bins.
pub(crate) struct TopologyNeighbours {
    topology: TopologyHandle<Arc<Identity>>,
}

impl TopologyNeighbours {
    pub(crate) fn new(topology: TopologyHandle<Arc<Identity>>) -> Self {
        Self { topology }
    }
}

impl NeighbourSource for TopologyNeighbours {
    fn targets(&self) -> Vec<SyncTarget> {
        let depth = self.topology.depth();
        // Scope to the routing table's deepest bin, matching `neighbors(depth)`;
        // bins above it hold no peers, so driving ranges there wastes a pass.
        let bins: Vec<Bin> = neighborhood_bins(depth, self.topology.max_bin()).collect();
        self.topology
            .neighbors(depth)
            .into_iter()
            .filter_map(|overlay| {
                let peer = self.topology.resolve_peer_id(&overlay)?;
                Some(SyncTarget {
                    peer,
                    overlay,
                    bins: bins.clone(),
                })
            })
            .collect()
    }
}
