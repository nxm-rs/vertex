//! Swarm-specific peer data for persistence via PeerRecord.

use serde::{Deserialize, Serialize};
use vertex_swarm_peer_score::PeerScoreSnapshot;
use vertex_net_peer_store::PeerRecord;
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_primitives::{OverlayAddress, SwarmNodeType};

use crate::ban::BanInfo;

/// Swarm-specific peer data stored as the `Data` field of `PeerRecord`.
///
/// Generic fields (timestamps, backoff, banned) live on `PeerRecord` itself.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SwarmPeerData {
    /// Full peer identity (includes multiaddrs for IP capability).
    pub peer: SwarmPeer,
    /// Node type (bootnode, client, storer).
    pub node_type: SwarmNodeType,
    /// Scoring metrics.
    pub scoring: PeerScoreSnapshot,
    /// Ban information if peer is banned (Swarm-specific metadata).
    pub ban_info: Option<BanInfo>,
}

/// Convenience alias for the full persistence record.
pub type SwarmPeerRecord = PeerRecord<OverlayAddress, SwarmPeerData>;

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_test_utils::test_swarm_peer;

    #[test]
    fn test_serialization() {
        let data = SwarmPeerData {
            peer: test_swarm_peer(1),
            node_type: SwarmNodeType::Storer,
            scoring: PeerScoreSnapshot::default(),
            ban_info: None,
        };

        let record = SwarmPeerRecord {
            id: OverlayAddress::from(*data.peer.overlay()),
            data,
            first_seen: 100,
            last_seen: 200,
            last_dial_attempt: 150,
            consecutive_failures: 3,
            is_banned: false,
        };

        let json = serde_json::to_string(&record).unwrap();
        let restored: SwarmPeerRecord = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.data.node_type, SwarmNodeType::Storer);
        assert_eq!(restored.first_seen, 100);
        assert_eq!(restored.last_seen, 200);
        assert_eq!(restored.last_dial_attempt, 150);
        assert_eq!(restored.consecutive_failures, 3);
    }
}
