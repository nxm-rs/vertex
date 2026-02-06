//! Swarm-specific extended peer state for NetPeerManager.

use serde::{Deserialize, Serialize};
use vertex_net_local::IpCapability;
use vertex_net_peers::NetPeerExt;
use vertex_swarm_peer::SwarmPeer;

/// Swarm-specific extended peer state.
///
/// Contains the full [`SwarmPeer`] identity (multiaddrs, signature, overlay, nonce,
/// ethereum_address) and computed IP capability. Peers are only added to the
/// manager when SwarmPeer is known (from handshake or Hive gossip).
#[derive(Debug, Clone)]
pub struct SwarmExt {
    /// Full peer identity (always present).
    pub peer: SwarmPeer,
    /// IP connectivity capability (computed from multiaddrs).
    pub ip_capability: IpCapability,
    /// Whether this peer runs as a full node.
    pub full_node: bool,
}

impl SwarmExt {
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
}

/// Serializable snapshot of SwarmExt for persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmExtSnapshot {
    /// Full peer identity.
    pub peer: SwarmPeer,
    /// IP capability.
    pub ip_capability: IpCapability,
    /// Whether this peer runs as a full node.
    pub full_node: bool,
}

impl NetPeerExt for SwarmExt {
    type Snapshot = SwarmExtSnapshot;

    fn snapshot(&self) -> Self::Snapshot {
        SwarmExtSnapshot {
            peer: self.peer.clone(),
            ip_capability: self.ip_capability,
            full_node: self.full_node,
        }
    }

    fn restore(&mut self, snapshot: &Self::Snapshot) {
        self.peer = snapshot.peer.clone();
        self.ip_capability = snapshot.ip_capability;
        self.full_node = snapshot.full_node;
    }

    fn from_snapshot(snapshot: &Self::Snapshot) -> Self {
        Self {
            peer: snapshot.peer.clone(),
            ip_capability: snapshot.ip_capability,
            full_node: snapshot.full_node,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256, Signature};

    fn test_swarm_peer() -> SwarmPeer {
        let multiaddrs = vec!["/ip4/127.0.0.1/tcp/1634".parse().unwrap()];
        SwarmPeer::from_validated(
            multiaddrs,
            Signature::test_signature(),
            B256::repeat_byte(1),
            B256::ZERO,
            Address::ZERO,
        )
    }

    #[test]
    fn test_new() {
        let peer = test_swarm_peer();
        let ext = SwarmExt::new(peer.clone(), true);

        assert_eq!(ext.swarm_peer(), &peer);
        assert!(ext.full_node);
        assert!(!ext.ip_capability.is_empty());
    }

    #[test]
    fn test_snapshot_roundtrip() {
        let peer = test_swarm_peer();
        let ext = SwarmExt::new(peer, true);

        let snapshot = ext.snapshot();
        let restored = SwarmExt::from_snapshot(&snapshot);

        assert_eq!(ext.peer, restored.peer);
        assert_eq!(ext.ip_capability, restored.ip_capability);
        assert_eq!(ext.full_node, restored.full_node);
    }
}
