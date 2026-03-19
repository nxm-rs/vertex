//! Mock topology implementations for testing.

use nectar_primitives::{ChunkAddress, SwarmAddress};
use std::sync::Arc;
use vertex_swarm_api::{
    SwarmIdentity, SwarmNodeType, SwarmTopologyBins, SwarmTopologyPeers, SwarmTopologyRouting,
    SwarmTopologyState, SwarmTopologyStats,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_primitives::OverlayAddress;

use crate::test_identity_arc;

/// A mock topology for testing node components.
///
/// Provides configurable peer counts and depth for testing
/// different network scenarios without needing a real P2P network.
#[derive(Clone)]
pub struct MockTopology {
    identity: Arc<Identity>,
    connected: usize,
    routing: usize,
    stored: usize,
    pending: usize,
    depth: u8,
}

impl Default for MockTopology {
    fn default() -> Self {
        Self {
            identity: test_identity_arc(),
            connected: 0,
            routing: 0,
            stored: 0,
            pending: 0,
            depth: 0,
        }
    }
}

impl std::fmt::Debug for MockTopology {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockTopology")
            .field("overlay", &self.identity.overlay_address())
            .field("connected", &self.connected)
            .field("routing", &self.routing)
            .field("depth", &self.depth)
            .finish()
    }
}

impl MockTopology {
    /// Create a new mock topology with the given parameters.
    pub fn new(connected: usize, routing: usize, depth: u8) -> Self {
        Self {
            identity: test_identity_arc(),
            connected,
            routing,
            stored: 0,
            pending: 0,
            depth,
        }
    }

    /// Create a mock topology with a specific identity.
    #[must_use]
    pub fn with_identity(mut self, identity: Arc<Identity>) -> Self {
        self.identity = identity;
        self
    }

    /// Set the number of connected peers.
    #[must_use]
    pub fn with_connected(mut self, connected: usize) -> Self {
        self.connected = connected;
        self
    }

    /// Set the number of routing peers.
    #[must_use]
    pub fn with_routing(mut self, routing: usize) -> Self {
        self.routing = routing;
        self
    }

    /// Set the number of stored peers.
    #[must_use]
    pub fn with_stored(mut self, stored: usize) -> Self {
        self.stored = stored;
        self
    }

    /// Set the number of pending connections.
    #[must_use]
    pub fn with_pending(mut self, pending: usize) -> Self {
        self.pending = pending;
        self
    }

    /// Set the topology depth.
    #[must_use]
    pub fn with_depth(mut self, depth: u8) -> Self {
        self.depth = depth;
        self
    }

    /// Get the overlay address as SwarmAddress.
    pub fn overlay(&self) -> SwarmAddress {
        self.identity.overlay_address()
    }

    /// Get the node type.
    pub fn node_type(&self) -> SwarmNodeType {
        self.identity.node_type()
    }
}

impl SwarmTopologyBins for MockTopology {
    fn bin_sizes(&self) -> Vec<(usize, usize)> {
        vec![(0, 0); 32]
    }
}

impl SwarmTopologyState for MockTopology {
    type Identity = Identity;

    fn identity(&self) -> &Self::Identity {
        self.identity.as_ref()
    }

    fn depth(&self) -> u8 {
        self.depth
    }
}

impl SwarmTopologyRouting for MockTopology {
    fn closest_to(&self, _address: &ChunkAddress, _count: usize) -> Vec<OverlayAddress> {
        Vec::new()
    }

    fn neighbors(&self, _depth: u8) -> Vec<OverlayAddress> {
        Vec::new()
    }
}

impl SwarmTopologyPeers for MockTopology {
    fn connected_peers_in_bin(&self, _po: u8) -> Vec<OverlayAddress> {
        Vec::new()
    }

    fn connected_peer_details_in_bin(
        &self,
        _po: u8,
    ) -> Vec<(OverlayAddress, Vec<libp2p::Multiaddr>)> {
        Vec::new()
    }
}

impl SwarmTopologyStats for MockTopology {
    fn connected_peers_count(&self) -> usize {
        self.connected
    }

    fn routing_peers_count(&self) -> usize {
        self.routing
    }

    fn pending_connections_count(&self) -> usize {
        self.pending
    }

    fn stored_peers_count(&self) -> usize {
        self.stored
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_topology_default() {
        let topo = MockTopology::default();

        assert_eq!(topo.connected_peers_count(), 0);
        assert_eq!(topo.routing_peers_count(), 0);
        assert_eq!(topo.stored_peers_count(), 0);
        assert_eq!(topo.depth(), 0);
    }

    #[test]
    fn test_mock_topology_new() {
        let topo = MockTopology::new(10, 50, 4);

        assert_eq!(topo.connected_peers_count(), 10);
        assert_eq!(topo.routing_peers_count(), 50);
        assert_eq!(topo.depth(), 4);
    }

    #[test]
    fn test_mock_topology_builder() {
        let topo = MockTopology::default()
            .with_connected(5)
            .with_routing(20)
            .with_stored(100)
            .with_depth(3);

        assert_eq!(topo.connected_peers_count(), 5);
        assert_eq!(topo.routing_peers_count(), 20);
        assert_eq!(topo.stored_peers_count(), 100);
        assert_eq!(topo.depth(), 3);
    }

    #[test]
    fn test_mock_topology_with_identity() {
        let identity = test_identity_arc();
        let overlay = identity.overlay_address();

        let topo = MockTopology::default().with_identity(identity);

        assert_eq!(topo.overlay(), overlay);
    }

    #[test]
    fn test_swarm_topology_trait() {
        let topo = MockTopology::new(5, 10, 2);

        assert_eq!(topo.depth(), 2);
        assert!(topo.neighbors(0).is_empty());
        assert_eq!(topo.bin_sizes().len(), 32);
    }
}
