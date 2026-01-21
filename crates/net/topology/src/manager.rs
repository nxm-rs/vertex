//! Peer lifecycle management.
//!
//! The peer manager tracks connection state, handles disconnections, and
//! maintains the desired peer count across Kademlia bins.

use std::collections::HashMap;
use vertex_primitives::OverlayAddress;

/// Connection state of a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerState {
    /// Attempting to connect.
    Connecting,

    /// Connected and active.
    Connected,

    /// Disconnected (may reconnect).
    Disconnected,

    /// Banned (will not reconnect).
    Banned,
}

/// Manages peer connections and lifecycle.
pub struct PeerManager {
    /// Peer states by overlay address.
    states: HashMap<OverlayAddress, PeerState>,

    /// Maximum number of peers to maintain.
    max_peers: usize,
}

impl PeerManager {
    /// Create a new peer manager.
    pub fn new(max_peers: usize) -> Self {
        Self {
            states: HashMap::new(),
            max_peers,
        }
    }

    /// Get the state of a peer.
    pub fn state(&self, overlay: &OverlayAddress) -> Option<PeerState> {
        self.states.get(overlay).copied()
    }

    /// Check if a peer is connected.
    pub fn is_connected(&self, overlay: &OverlayAddress) -> bool {
        self.states.get(overlay) == Some(&PeerState::Connected)
    }

    /// Mark a peer as connecting.
    pub fn set_connecting(&mut self, overlay: OverlayAddress) {
        self.states.insert(overlay, PeerState::Connecting);
    }

    /// Mark a peer as connected.
    pub fn set_connected(&mut self, overlay: OverlayAddress) {
        self.states.insert(overlay, PeerState::Connected);
    }

    /// Mark a peer as disconnected.
    pub fn set_disconnected(&mut self, overlay: OverlayAddress) {
        self.states.insert(overlay, PeerState::Disconnected);
    }

    /// Ban a peer (prevent reconnection).
    pub fn ban(&mut self, overlay: OverlayAddress) {
        self.states.insert(overlay, PeerState::Banned);
    }

    /// Check if a peer is banned.
    pub fn is_banned(&self, overlay: &OverlayAddress) -> bool {
        self.states.get(overlay) == Some(&PeerState::Banned)
    }

    /// Remove a peer from tracking.
    pub fn remove(&mut self, overlay: &OverlayAddress) {
        self.states.remove(overlay);
    }

    /// Get the count of peers in each state.
    pub fn counts(&self) -> PeerCounts {
        let mut counts = PeerCounts::default();
        for state in self.states.values() {
            match state {
                PeerState::Connecting => counts.connecting += 1,
                PeerState::Connected => counts.connected += 1,
                PeerState::Disconnected => counts.disconnected += 1,
                PeerState::Banned => counts.banned += 1,
            }
        }
        counts
    }

    /// Check if we can accept more connections.
    pub fn can_accept_more(&self) -> bool {
        self.counts().connected < self.max_peers
    }

    /// Get the number of connected peers.
    pub fn connected_count(&self) -> usize {
        self.counts().connected
    }

    /// Iterate over all connected peers.
    pub fn connected_peers(&self) -> impl Iterator<Item = &OverlayAddress> {
        self.states
            .iter()
            .filter(|(_, state)| **state == PeerState::Connected)
            .map(|(overlay, _)| overlay)
    }
}

/// Counts of peers in each state.
#[derive(Debug, Clone, Default)]
pub struct PeerCounts {
    /// Peers we're attempting to connect to.
    pub connecting: usize,

    /// Connected peers.
    pub connected: usize,

    /// Disconnected peers (may reconnect).
    pub disconnected: usize,

    /// Banned peers.
    pub banned: usize,
}

impl PeerCounts {
    /// Total peers being tracked.
    pub fn total(&self) -> usize {
        self.connecting + self.connected + self.disconnected + self.banned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_overlay(b: u8) -> OverlayAddress {
        let mut bytes = [0u8; 32];
        bytes[0] = b;
        OverlayAddress::new(bytes)
    }

    #[test]
    fn test_peer_lifecycle() {
        let mut manager = PeerManager::new(50);

        let peer = make_overlay(1);

        // Initially unknown
        assert_eq!(manager.state(&peer), None);

        // Connecting
        manager.set_connecting(peer.clone());
        assert_eq!(manager.state(&peer), Some(PeerState::Connecting));

        // Connected
        manager.set_connected(peer.clone());
        assert!(manager.is_connected(&peer));

        // Disconnected
        manager.set_disconnected(peer.clone());
        assert!(!manager.is_connected(&peer));

        // Banned
        manager.ban(peer.clone());
        assert!(manager.is_banned(&peer));
    }

    #[test]
    fn test_counts() {
        let mut manager = PeerManager::new(50);

        manager.set_connected(make_overlay(1));
        manager.set_connected(make_overlay(2));
        manager.set_connecting(make_overlay(3));
        manager.ban(make_overlay(4));

        let counts = manager.counts();
        assert_eq!(counts.connected, 2);
        assert_eq!(counts.connecting, 1);
        assert_eq!(counts.banned, 1);
        assert_eq!(counts.total(), 4);
    }
}
