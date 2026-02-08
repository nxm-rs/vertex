//! Persistence snapshot for Swarm peers.

use serde::{Deserialize, Serialize};
use vertex_net_local::IpCapability;
use vertex_net_peer_score::PeerScoreSnapshot;
use vertex_swarm_peer::SwarmPeer;

use crate::ban::BanInfo;

/// Serializable snapshot of peer state for persistence.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SwarmPeerSnapshot {
    /// Full peer identity.
    pub peer: SwarmPeer,
    /// IP connectivity capability.
    pub ip_capability: IpCapability,
    /// Whether this peer runs as a full node.
    pub full_node: bool,
    /// Scoring metrics.
    pub scoring: PeerScoreSnapshot,
    /// Ban information if peer is banned.
    pub ban_info: Option<BanInfo>,
    /// Unix timestamp when peer was first seen.
    pub first_seen: u64,
    /// Unix timestamp when peer was last seen.
    pub last_seen: u64,
}

impl SwarmPeerSnapshot {
    /// Create a new snapshot.
    pub fn new(
        peer: SwarmPeer,
        ip_capability: IpCapability,
        full_node: bool,
        scoring: PeerScoreSnapshot,
        ban_info: Option<BanInfo>,
        first_seen: u64,
        last_seen: u64,
    ) -> Self {
        Self {
            peer,
            ip_capability,
            full_node,
            scoring,
            ban_info,
            first_seen,
            last_seen,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_test_utils::test_swarm_peer;

    #[test]
    fn test_serialization() {
        let snapshot = SwarmPeerSnapshot {
            peer: test_swarm_peer(1),
            ip_capability: IpCapability::default(),
            full_node: true,
            scoring: PeerScoreSnapshot::default(),
            ban_info: None,
            first_seen: 100,
            last_seen: 200,
        };

        let json = serde_json::to_string(&snapshot).unwrap();
        let restored: SwarmPeerSnapshot = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.full_node, snapshot.full_node);
        assert_eq!(restored.first_seen, snapshot.first_seen);
        assert_eq!(restored.last_seen, snapshot.last_seen);
    }
}
