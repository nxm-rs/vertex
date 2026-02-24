//! Swarm-specific peer registry wrapping the generic PeerRegistry.

use std::collections::HashMap;

use libp2p::{Multiaddr, PeerId, swarm::ConnectionId};
use parking_lot::RwLock;
use vertex_net_peer_registry::{ActivateResult, ConnectionState, PeerRegistry};
use vertex_swarm_primitives::OverlayAddress;

use crate::reason::DialReason;

/// Swarm-specific peer registry with dial reason tracking.
pub struct SwarmPeerRegistry {
    inner: PeerRegistry<OverlayAddress, Option<DialReason>>,
    /// Agent versions received via libp2p identify (keyed by PeerId).
    agent_versions: RwLock<HashMap<PeerId, String>>,
}

impl Default for SwarmPeerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SwarmPeerRegistry {
    pub fn new() -> Self {
        Self {
            inner: PeerRegistry::new(),
            agent_versions: RwLock::new(HashMap::new()),
        }
    }

    pub fn get(&self, overlay: &OverlayAddress) -> Option<ConnectionState<OverlayAddress, Option<DialReason>>> {
        self.inner.get(overlay)
    }

    pub fn active_connection_id(&self, overlay: &OverlayAddress) -> Option<ConnectionId> {
        self.inner.active_connection_id(overlay)
    }

    pub fn resolve_overlay(&self, peer_id: &PeerId) -> Option<OverlayAddress> {
        self.inner.resolve_id(peer_id)
    }

    pub fn contains_peer(&self, peer_id: &PeerId) -> bool {
        self.inner.contains_peer(peer_id)
    }

    /// Check if a peer's dial reason is Verification.
    pub fn is_verification(&self, peer_id: &PeerId) -> bool {
        self.inner
            .get_by_peer_id(peer_id)
            .and_then(|s| *s.reason())
            == Some(DialReason::Verification)
    }

    pub fn resolve_peer_id(&self, overlay: &OverlayAddress) -> Option<PeerId> {
        self.inner.resolve_peer_id(overlay)
    }

    /// Start dialing a peer with known overlay. Returns all addresses for DialOpts.
    #[must_use]
    pub fn start_dial(
        &self,
        peer_id: PeerId,
        overlay: OverlayAddress,
        addrs: Vec<Multiaddr>,
        reason: DialReason,
    ) -> Option<Vec<Multiaddr>> {
        self.inner.start_dial(peer_id, overlay, addrs, Some(reason))
    }

    /// Start dialing without known overlay (for bootnodes/commands).
    /// Returns all addresses for DialOpts.
    #[must_use]
    pub fn start_dial_unknown_overlay(
        &self,
        peer_id: PeerId,
        addrs: Vec<Multiaddr>,
        reason: DialReason,
    ) -> Option<Vec<Multiaddr>> {
        self.inner.start_dial_unknown_id(peer_id, addrs, Some(reason))
    }

    /// Complete a dial attempt (success or failure). Returns state for diagnostics.
    pub fn complete_dial(&self, peer_id: &PeerId) -> Option<ConnectionState<OverlayAddress, Option<DialReason>>> {
        self.inner.complete_dial(peer_id)
    }

    /// Transition from Dialing to Connected after TCP/QUIC connection established.
    pub fn connection_established(
        &self,
        peer_id: PeerId,
        connection_id: ConnectionId,
    ) -> Option<ConnectionState<OverlayAddress, Option<DialReason>>> {
        self.inner.connection_established(peer_id, connection_id)
    }

    /// Register inbound connection in Connected state (awaiting overlay from handshake).
    pub fn inbound_connection(
        &self,
        peer_id: PeerId,
        connection_id: ConnectionId,
    ) -> ConnectionState<OverlayAddress, Option<DialReason>> {
        self.inner.inbound_connection(peer_id, connection_id)
    }

    /// Activate a connection after handshake provides the overlay address.
    pub fn handshake_completed(
        &self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        overlay: OverlayAddress,
    ) -> ActivateResult<OverlayAddress> {
        self.inner.activate(peer_id, connection_id, overlay)
    }

    /// Get the dial reason for an overlay (if known).
    pub fn dial_reason(&self, overlay: &OverlayAddress) -> Option<DialReason> {
        self.inner.get(overlay).and_then(|s| *s.reason())
    }

    /// Store agent version from libp2p identify protocol.
    pub fn set_agent_version(&self, peer_id: &PeerId, agent_version: String) {
        self.agent_versions.write().insert(*peer_id, agent_version);
    }

    /// Get agent version for a peer by PeerId.
    pub fn agent_version(&self, peer_id: &PeerId) -> Option<String> {
        self.agent_versions.read().get(peer_id).cloned()
    }

    /// Get agent version for a peer by overlay address.
    pub fn agent_version_by_overlay(&self, overlay: &OverlayAddress) -> Option<String> {
        let peer_id = self.resolve_peer_id(overlay)?;
        self.agent_versions.read().get(&peer_id).cloned()
    }

    pub fn get_by_peer_id(&self, peer_id: &PeerId) -> Option<ConnectionState<OverlayAddress, Option<DialReason>>> {
        self.inner.get_by_peer_id(peer_id)
    }

    #[must_use]
    pub fn active_peers(&self) -> Vec<OverlayAddress> {
        self.inner.active_ids()
    }

    pub fn active_count(&self) -> usize {
        self.inner.active_count()
    }

    pub fn pending_count(&self) -> usize {
        self.inner.pending_count()
    }

    /// Get PeerIds of pending connections that have exceeded the timeout.
    #[must_use]
    pub fn stale_pending(&self, timeout: std::time::Duration) -> Vec<PeerId> {
        self.inner.stale_pending(timeout)
    }

    /// Remove peer and clean up agent version.
    pub fn disconnected(&self, peer_id: &PeerId) -> Option<ConnectionState<OverlayAddress, Option<DialReason>>> {
        let state = self.inner.disconnected(peer_id)?;
        self.agent_versions.write().remove(peer_id);
        Some(state)
    }

    /// Get statistics including memory estimation.
    #[must_use]
    pub fn stats(&self) -> SwarmPeerRegistryStats {
        let inner_stats = self.inner.stats();
        let agent_versions_count = self.agent_versions.read().len();

        // Memory estimation:
        // - ConnectionState entry: ~264 bytes (overlay 32, PeerId 38, ConnectionId 8, state enum ~100, addrs ~80, reason ~8)
        // - Agent version entry: ~128 bytes (PeerId 38, String ~90 avg)
        const CONNECTION_ENTRY_SIZE: usize = 264;
        const AGENT_VERSION_SIZE: usize = 128;

        let estimated_memory_bytes = inner_stats.total_entries * CONNECTION_ENTRY_SIZE
            + agent_versions_count * AGENT_VERSION_SIZE;

        SwarmPeerRegistryStats {
            active_count: inner_stats.active_count,
            pending_count: inner_stats.pending_count,
            total_entries: inner_stats.total_entries,
            agent_versions_count,
            estimated_memory_bytes,
        }
    }
}

impl vertex_net_peer_registry::PeerResolver for SwarmPeerRegistry {
    type Id = OverlayAddress;

    fn resolve_id(&self, peer_id: &PeerId) -> Option<OverlayAddress> {
        self.inner.resolve_id(peer_id)
    }

    fn resolve_peer_id(&self, overlay: &OverlayAddress) -> Option<PeerId> {
        self.inner.resolve_peer_id(overlay)
    }
}

/// Statistics about the connection registry state.
#[derive(Debug, Clone, Copy)]
pub struct SwarmPeerRegistryStats {
    /// Number of active (connected) peers.
    pub active_count: usize,
    /// Number of pending (dialing/connected) connections.
    pub pending_count: usize,
    /// Total entries in the registry.
    pub total_entries: usize,
    /// Number of stored agent versions.
    pub agent_versions_count: usize,
    /// Estimated memory usage in bytes.
    pub estimated_memory_bytes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_test_utils::{make_overlay as test_overlay, test_peer_id};

    fn test_addr(port: u16) -> Multiaddr {
        format!("/ip4/127.0.0.1/tcp/{}", port).parse().unwrap()
    }

    fn test_connection_id(n: u8) -> ConnectionId {
        ConnectionId::new_unchecked(n as usize)
    }

    #[test]
    fn test_start_dial_with_reason() {
        let registry = SwarmPeerRegistry::new();
        let overlay = test_overlay(1);
        let peer_id = test_peer_id(1);
        let addrs = vec![test_addr(9000)];

        let result = registry.start_dial(peer_id, overlay, addrs.clone(), DialReason::Discovery);
        assert_eq!(result, Some(addrs));
        assert_eq!(registry.dial_reason(&overlay), Some(DialReason::Discovery));
    }

    #[test]
    fn test_disconnect_cleans_up_reason() {
        let registry = SwarmPeerRegistry::new();
        let overlay = test_overlay(1);
        let peer_id = test_peer_id(1);
        let conn_id = test_connection_id(1);

        let _ = registry.start_dial(peer_id, overlay, vec![test_addr(9000)], DialReason::Command);
        let _ = registry.connection_established(peer_id, conn_id);
        registry.handshake_completed(peer_id, conn_id, overlay);

        assert_eq!(registry.dial_reason(&overlay), Some(DialReason::Command));

        registry.disconnected(&peer_id);
        assert_eq!(registry.dial_reason(&overlay), None);
    }

    #[test]
    fn test_is_verification() {
        let registry = SwarmPeerRegistry::new();
        let overlay = test_overlay(1);
        let peer_id = test_peer_id(1);

        // Not in registry -> false
        assert!(!registry.is_verification(&peer_id));

        // Discovery dial -> false
        let _ = registry.start_dial(peer_id, overlay, vec![test_addr(9000)], DialReason::Discovery);
        assert!(!registry.is_verification(&peer_id));
        registry.disconnected(&peer_id);

        // Verification dial -> true
        let peer_id2 = test_peer_id(2);
        let overlay2 = test_overlay(2);
        let _ = registry.start_dial(peer_id2, overlay2, vec![test_addr(9001)], DialReason::Verification);
        assert!(registry.is_verification(&peer_id2));

        // After disconnect -> false
        registry.disconnected(&peer_id2);
        assert!(!registry.is_verification(&peer_id2));
    }

    #[test]
    fn test_reason_carried_through_unknown_overlay_dial() {
        let registry = SwarmPeerRegistry::new();
        let peer_id = test_peer_id(1);
        let conn_id = test_connection_id(1);
        let overlay = test_overlay(1);

        // Start unknown-overlay dial with reason
        let _ = registry.start_dial_unknown_overlay(peer_id, vec![test_addr(9000)], DialReason::Bootnode);

        // Verify reason is on the state
        let state = registry.get_by_peer_id(&peer_id).unwrap();
        assert_eq!(*state.reason(), Some(DialReason::Bootnode));

        // Reason carries through connection_established
        let _ = registry.connection_established(peer_id, conn_id);
        let state = registry.get_by_peer_id(&peer_id).unwrap();
        assert_eq!(*state.reason(), Some(DialReason::Bootnode));

        // Reason carries through activation (migration from Pending to Known)
        registry.handshake_completed(peer_id, conn_id, overlay);
        let state = registry.get(&overlay).unwrap();
        assert_eq!(*state.reason(), Some(DialReason::Bootnode));
        assert_eq!(registry.dial_reason(&overlay), Some(DialReason::Bootnode));
    }

    #[test]
    fn test_stale_pending_covers_unknown_dials() {
        use std::time::Duration;

        let registry = SwarmPeerRegistry::new();
        let peer_id = test_peer_id(1);
        let addrs = vec![test_addr(9000)];

        let _ = registry.start_dial_unknown_overlay(peer_id, addrs, DialReason::Command);

        // With zero timeout, dial should be stale immediately
        let stale = registry.stale_pending(Duration::from_secs(0));
        assert_eq!(stale.len(), 1);
        assert!(stale.contains(&peer_id));

        // With large timeout, no dials should be stale
        let stale = registry.stale_pending(Duration::from_secs(3600));
        assert!(stale.is_empty());
    }
}
