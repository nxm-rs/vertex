//! Shared test infrastructure for topology tests.

use std::sync::Arc;

use vertex_swarm_api::SwarmNodeType;
use vertex_swarm_peer_manager::PeerManager;
use vertex_swarm_test_utils::{MockIdentity, test_overlay, test_swarm_peer};
use vertex_swarm_primitives::OverlayAddress;

use crate::behaviour::ConnectionRegistry;

pub(crate) struct TopologyTestContext {
    pub local_overlay: OverlayAddress,
    pub peer_manager: Arc<PeerManager<MockIdentity>>,
    pub connection_registry: Arc<ConnectionRegistry>,
}

impl TopologyTestContext {
    pub(crate) fn new() -> Self {
        let local = test_overlay(0);
        let identity = MockIdentity::with_overlay(local);
        let pm = PeerManager::new(&identity);
        let cr = Arc::new(ConnectionRegistry::new());
        Self {
            local_overlay: local,
            peer_manager: pm,
            connection_registry: cr,
        }
    }

    pub(crate) fn with_peers(self) -> Self {
        for n in 1..=10 {
            self.peer_manager
                .on_peer_ready(test_swarm_peer(n), SwarmNodeType::Storer);
        }
        self
    }
}
