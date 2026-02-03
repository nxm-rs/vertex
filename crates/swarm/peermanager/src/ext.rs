//! Swarm-specific extended peer state for NetPeerManager.
//!
//! [`SwarmExt`] holds the Swarm protocol-specific data that extends the generic
//! [`PeerState`] from vertex-net-peers. This includes the full [`SwarmPeer`] identity
//! (BzzAddress components) and computed IP capability.

use serde::{Deserialize, Serialize};
use vertex_net_peer::IpCapability;
use vertex_net_peers::NetPeerExt;
use vertex_swarm_peer::SwarmPeer;

/// Swarm-specific extended peer state.
///
/// Contains the full [`SwarmPeer`] identity (multiaddrs, signature, overlay, nonce,
/// ethereum_address) once available, plus computed IP capability.
///
/// The `peer` field is `None` until we receive the full identity via:
/// - Handshake (bzz protocol)
/// - Hive gossip
///
/// Basic peer tracking (connection state, score, latency) is handled by the generic
/// [`PeerState`] in vertex-net-peers. This struct holds Swarm-specific additions.
#[derive(Debug, Clone)]
pub struct SwarmExt {
    /// Full peer identity. None until handshake or Hive provides it.
    pub peer: Option<SwarmPeer>,
    /// IP connectivity capability (computed from multiaddrs).
    pub ip_capability: IpCapability,
    /// Whether this peer runs as a full node (stores/serves content).
    pub full_node: bool,
}

impl Default for SwarmExt {
    fn default() -> Self {
        Self {
            peer: None,
            ip_capability: IpCapability::None,
            full_node: false,
        }
    }
}

impl SwarmExt {
    /// Create with a known SwarmPeer identity.
    pub fn with_peer(peer: SwarmPeer, full_node: bool) -> Self {
        let ip_capability = IpCapability::from_addrs(peer.multiaddrs());
        Self {
            peer: Some(peer),
            ip_capability,
            full_node,
        }
    }

    /// Set the peer identity (after handshake/Hive).
    pub fn set_peer(&mut self, peer: SwarmPeer) {
        self.ip_capability = IpCapability::from_addrs(peer.multiaddrs());
        self.peer = Some(peer);
    }

    /// Check if we have the full peer identity.
    pub fn has_identity(&self) -> bool {
        self.peer.is_some()
    }

    /// Get the SwarmPeer if available.
    pub fn swarm_peer(&self) -> Option<&SwarmPeer> {
        self.peer.as_ref()
    }

    /// Update IP capability (e.g., when multiaddrs change).
    pub fn update_ip_capability(&mut self, addrs: &[libp2p::Multiaddr]) {
        self.ip_capability = IpCapability::from_addrs(addrs);
    }
}

/// Serializable snapshot of SwarmExt for persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmExtSnapshot {
    /// Full peer identity (serializable).
    pub peer: Option<SwarmPeer>,
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
    fn test_default() {
        let ext = SwarmExt::default();
        assert!(ext.peer.is_none());
        assert_eq!(ext.ip_capability, IpCapability::None);
        assert!(!ext.has_identity());
        assert!(!ext.full_node);
    }

    #[test]
    fn test_with_peer() {
        let peer = test_swarm_peer();
        let ext = SwarmExt::with_peer(peer.clone(), true);

        assert!(ext.has_identity());
        assert_eq!(ext.swarm_peer(), Some(&peer));
        assert!(ext.full_node);
        // Should have computed IP capability from multiaddrs
        assert_ne!(ext.ip_capability, IpCapability::None);
    }

    #[test]
    fn test_set_peer() {
        let mut ext = SwarmExt::default();
        assert!(!ext.has_identity());

        let peer = test_swarm_peer();
        ext.set_peer(peer.clone());

        assert!(ext.has_identity());
        assert_eq!(ext.swarm_peer(), Some(&peer));
    }

    #[test]
    fn test_snapshot_roundtrip() {
        let peer = test_swarm_peer();
        let ext = SwarmExt::with_peer(peer, true);

        let snapshot = ext.snapshot();
        let mut restored = SwarmExt::default();
        restored.restore(&snapshot);

        assert_eq!(ext.peer, restored.peer);
        assert_eq!(ext.ip_capability, restored.ip_capability);
        assert_eq!(ext.full_node, restored.full_node);
    }
}
