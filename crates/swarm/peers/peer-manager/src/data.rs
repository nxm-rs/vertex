//! Swarm-specific peer data.

use serde::{Deserialize, Serialize};
use vertex_net_local::IpCapability;
use vertex_swarm_peer::SwarmPeer;

/// Swarm-specific peer data.
///
/// Contains the full [`SwarmPeer`] identity (multiaddrs, signature, overlay, nonce,
/// ethereum_address) and computed IP capability. Peers are only added to the
/// manager when SwarmPeer is known (from handshake or Hive gossip).
#[derive(Debug, Clone)]
pub struct SwarmPeerData {
    peer: SwarmPeer,
    ip_capability: IpCapability,
    full_node: bool,
}

impl SwarmPeerData {
    /// Create with a SwarmPeer identity.
    pub fn new(peer: SwarmPeer, full_node: bool) -> Self {
        let ip_capability = IpCapability::from_addrs(peer.multiaddrs());
        Self {
            peer,
            ip_capability,
            full_node,
        }
    }

    /// Get the SwarmPeer.
    pub fn swarm_peer(&self) -> &SwarmPeer {
        &self.peer
    }

    /// Get the IP capability.
    pub fn ip_capability(&self) -> IpCapability {
        self.ip_capability
    }

    /// Check if this peer is a full node.
    pub fn is_full_node(&self) -> bool {
        self.full_node
    }
}

/// Serializable snapshot of SwarmPeerData for persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmPeerDataSnapshot {
    /// Full peer identity.
    pub peer: SwarmPeer,
    /// IP capability.
    pub ip_capability: IpCapability,
    /// Whether this peer runs as a full node.
    pub full_node: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_test_utils::test_swarm_peer;

    #[test]
    fn test_new() {
        let peer = test_swarm_peer(1);
        let data = SwarmPeerData::new(peer.clone(), true);

        assert_eq!(data.swarm_peer(), &peer);
        assert!(data.is_full_node());
        assert!(!data.ip_capability().is_empty());
    }
}
