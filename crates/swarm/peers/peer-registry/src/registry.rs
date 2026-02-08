//! Swarm-specific peer registry wrapping the generic PeerRegistry.

use std::collections::HashSet;

use libp2p::{Multiaddr, PeerId, swarm::ConnectionId};
use parking_lot::RwLock;
use vertex_net_peer_registry::{ActivateResult, ConnectionState, PeerRegistry};
use vertex_swarm_primitives::OverlayAddress;

use crate::reason::DialReason;

/// Swarm-specific peer registry with dial reason tracking and gossip disconnect pending.
pub struct SwarmPeerRegistry {
    inner: PeerRegistry<OverlayAddress>,
    dial_reasons: RwLock<std::collections::HashMap<OverlayAddress, DialReason>>,
    gossip_disconnect_pending: RwLock<HashSet<OverlayAddress>>,
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
            dial_reasons: RwLock::new(std::collections::HashMap::new()),
            gossip_disconnect_pending: RwLock::new(HashSet::new()),
        }
    }

    pub fn get(&self, overlay: &OverlayAddress) -> Option<ConnectionState<OverlayAddress>> {
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

    pub fn resolve_peer_id(&self, overlay: &OverlayAddress) -> Option<PeerId> {
        self.inner.resolve_peer_id(overlay)
    }

    /// Start dialing a peer with known overlay. Returns all addresses for DialOpts.
    pub fn start_dial(
        &self,
        peer_id: PeerId,
        overlay: OverlayAddress,
        addrs: Vec<Multiaddr>,
        reason: DialReason,
    ) -> Option<Vec<Multiaddr>> {
        let result = self.inner.start_dial(peer_id, overlay, addrs)?;
        self.dial_reasons.write().insert(overlay, reason);
        Some(result)
    }

    /// Start dialing without known overlay (for bootnodes/commands).
    /// Returns all addresses for DialOpts.
    pub fn start_dial_unknown_overlay(
        &self,
        peer_id: PeerId,
        addrs: Vec<Multiaddr>,
        reason: DialReason,
    ) -> Option<Vec<Multiaddr>> {
        let result = self
            .inner
            .start_dial_unknown_id(peer_id, addrs, sentinel_from_peer_id)?;

        // Track the reason using the sentinel
        let sentinel = sentinel_from_peer_id(&peer_id);
        self.dial_reasons.write().insert(sentinel, reason);

        Some(result)
    }

    /// Complete a dial attempt (success or failure). Returns state for diagnostics.
    pub fn complete_dial(&self, peer_id: &PeerId) -> Option<ConnectionState<OverlayAddress>> {
        let state = self.inner.complete_dial(peer_id)?;
        if let Some(overlay) = state.id() {
            self.dial_reasons.write().remove(&overlay);
        }
        Some(state)
    }

    /// Transition from Dialing to Handshaking after TCP/QUIC connection established.
    pub fn connection_established(
        &self,
        peer_id: PeerId,
        connection_id: ConnectionId,
    ) -> Option<ConnectionState<OverlayAddress>> {
        self.inner.connection_established(peer_id, connection_id)
    }

    /// Handle inbound connection (goes directly to Handshaking).
    pub fn inbound_connection(
        &self,
        peer_id: PeerId,
        connection_id: ConnectionId,
    ) -> ConnectionState<OverlayAddress> {
        self.inner
            .inbound_connection(peer_id, connection_id, sentinel_from_peer_id)
    }

    /// Complete handshake and transition to Active state.
    pub fn handshake_completed(
        &self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        overlay: OverlayAddress,
    ) -> ActivateResult<OverlayAddress> {
        // Clean up sentinel dial reason if present
        let sentinel = sentinel_from_peer_id(&peer_id);
        if let Some(reason) = self.dial_reasons.write().remove(&sentinel) {
            // Transfer reason to real overlay
            self.dial_reasons.write().insert(overlay, reason);
        }

        self.inner.handshake_completed(peer_id, connection_id, overlay)
    }

    /// Get the dial reason for an overlay (if known).
    pub fn dial_reason(&self, overlay: &OverlayAddress) -> Option<DialReason> {
        self.dial_reasons.read().get(overlay).copied()
    }

    /// Mark an overlay for gossip disconnect (bin at capacity).
    pub fn mark_gossip_disconnect_pending(&self, overlay: &OverlayAddress) -> bool {
        if self.get(overlay).map(|s| s.is_active()).unwrap_or(false) {
            self.gossip_disconnect_pending.write().insert(*overlay);
            true
        } else {
            false
        }
    }

    pub fn is_gossip_disconnect_pending(&self, overlay: &OverlayAddress) -> bool {
        self.gossip_disconnect_pending.read().contains(overlay)
    }

    pub fn get_by_peer_id(&self, peer_id: &PeerId) -> Option<ConnectionState<OverlayAddress>> {
        self.inner.get_by_peer_id(peer_id)
    }

    pub fn complete_dial_by_peer_id(
        &self,
        peer_id: &PeerId,
    ) -> Option<ConnectionState<OverlayAddress>> {
        let state = self.inner.complete_dial_by_peer_id(peer_id)?;
        if let Some(overlay) = state.id() {
            self.dial_reasons.write().remove(&overlay);
        }
        Some(state)
    }

    pub fn active_peers(&self) -> Vec<OverlayAddress> {
        self.inner.active_ids()
    }

    pub fn active_count(&self) -> usize {
        self.inner.active_count()
    }

    pub fn pending_count(&self) -> usize {
        self.inner.pending_count()
    }

    pub fn disconnected(&self, peer_id: &PeerId) -> Option<ConnectionState<OverlayAddress>> {
        let state = self.inner.disconnected(peer_id)?;
        if let Some(overlay) = state.id() {
            self.dial_reasons.write().remove(&overlay);
            self.gossip_disconnect_pending.write().remove(&overlay);
        }
        Some(state)
    }
}

/// Create a sentinel overlay from a peer ID (for bootnodes/commands).
fn sentinel_from_peer_id(peer_id: &PeerId) -> OverlayAddress {
    let mut bytes = [0u8; 32];
    let peer_bytes = peer_id.to_bytes();
    let copy_len = peer_bytes.len().min(32);
    bytes[..copy_len].copy_from_slice(&peer_bytes[..copy_len]);
    OverlayAddress::from(bytes)
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
    fn test_gossip_disconnect_pending() {
        let registry = SwarmPeerRegistry::new();
        let overlay = test_overlay(1);
        let peer_id = test_peer_id(1);
        let conn_id = test_connection_id(1);

        registry.start_dial(peer_id, overlay, vec![test_addr(9000)], DialReason::Discovery);
        registry.connection_established(peer_id, conn_id);
        registry.handshake_completed(peer_id, conn_id, overlay);

        assert!(!registry.is_gossip_disconnect_pending(&overlay));
        assert!(registry.mark_gossip_disconnect_pending(&overlay));
        assert!(registry.is_gossip_disconnect_pending(&overlay));
    }

    #[test]
    fn test_disconnect_cleans_up_reason() {
        let registry = SwarmPeerRegistry::new();
        let overlay = test_overlay(1);
        let peer_id = test_peer_id(1);
        let conn_id = test_connection_id(1);

        registry.start_dial(peer_id, overlay, vec![test_addr(9000)], DialReason::Command);
        registry.connection_established(peer_id, conn_id);
        registry.handshake_completed(peer_id, conn_id, overlay);

        assert_eq!(registry.dial_reason(&overlay), Some(DialReason::Command));

        registry.disconnected(&peer_id);
        assert_eq!(registry.dial_reason(&overlay), None);
    }
}
