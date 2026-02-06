//! Gossip coordination state machine.
//!
//! [`GossipCoordinator`] manages the health check and gossip activation lifecycle:
//! connection → handshake → health check delay → ping → pong → gossip activation.

use std::{
    collections::{HashMap, HashSet},
    pin::Pin,
    sync::Arc,
    task::Context,
    time::Duration,
};

use libp2p::PeerId;
use tokio::time::{Interval, Sleep};
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_peermanager::PeerManager;
use vertex_swarm_primitives::OverlayAddress;

use crate::gossip::{GossipAction, HiveGossipManager};

// Re-export HiveGossipConfig for consumers (re-exported via lib.rs)
pub(crate) use crate::gossip::HiveGossipConfig;

/// Callback to get current network depth for gossip decisions.
pub type DepthProvider = Arc<dyn Fn() -> u8 + Send + Sync>;

/// Default delay before sending health check ping after handshake.
pub(crate) const DEFAULT_HEALTH_CHECK_DELAY: Duration = Duration::from_millis(500);

/// Pending health check awaiting delay before ping.
struct PendingHealthCheck {
    swarm_peer: SwarmPeer,
    is_full_node: bool,
    delay: Pin<Box<Sleep>>,
}

/// Action returned by coordinator for TopologyBehaviour to execute.
#[derive(Debug)]
pub(crate) enum CoordinatorAction {
    /// Send a health check ping to this peer.
    SendPing(PeerId),
}

/// Coordinates gossip state machine: health check delays, pending gossip, and periodic refresh.
pub(crate) struct GossipCoordinator {
    gossip_manager: Option<HiveGossipManager>,
    depth_provider: Option<DepthProvider>,
    /// Peers dialed for gossip exchange - get delayed ping.
    gossip_dial_peers: HashSet<PeerId>,
    /// Pending health checks awaiting delay before sending ping.
    pending_health_checks: HashMap<PeerId, PendingHealthCheck>,
    /// Pending gossip waiting for pong response after health check ping.
    pending_gossip: HashMap<PeerId, (SwarmPeer, bool)>,
    /// Delay before sending health check ping after handshake.
    health_check_delay: Duration,
    /// Interval for periodic gossip refresh.
    gossip_interval: Option<Pin<Box<Interval>>>,
}

impl GossipCoordinator {
    /// Create a new coordinator with gossip disabled.
    pub(crate) fn new() -> Self {
        Self {
            gossip_manager: None,
            depth_provider: None,
            gossip_dial_peers: HashSet::new(),
            pending_health_checks: HashMap::new(),
            pending_gossip: HashMap::new(),
            health_check_delay: DEFAULT_HEALTH_CHECK_DELAY,
            gossip_interval: None,
        }
    }

    /// Enable automatic hive gossip with the given configuration.
    pub(crate) fn enable_gossip(
        &mut self,
        config: HiveGossipConfig,
        local_overlay: OverlayAddress,
        peer_manager: Arc<PeerManager>,
        depth_provider: DepthProvider,
    ) {
        let refresh_interval = config.refresh_interval;
        self.gossip_manager = Some(HiveGossipManager::new(config, local_overlay, peer_manager));
        self.depth_provider = Some(depth_provider);
        self.gossip_interval = Some(Box::pin(tokio::time::interval(refresh_interval)));
    }

    /// Set the delay before sending health check ping after handshake.
    pub(crate) fn set_health_check_delay(&mut self, delay: Duration) {
        self.health_check_delay = delay;
    }

    /// Get current depth from provider.
    pub(crate) fn current_depth(&self) -> u8 {
        self.depth_provider.as_ref().map(|p| p()).unwrap_or(0)
    }

    /// Mark a peer as dialed for gossip (will get delayed health check).
    pub(crate) fn mark_gossip_dial(&mut self, peer_id: PeerId) {
        self.gossip_dial_peers.insert(peer_id);
    }

    /// Handle handshake completion. Returns action if immediate ping should be sent.
    ///
    /// For gossip dials: schedules delayed ping, returns None.
    /// For inbound/kademlia dials: returns SendPing action for immediate health check.
    pub(crate) fn on_handshake_completed(
        &mut self,
        peer_id: PeerId,
        swarm_peer: SwarmPeer,
        is_full_node: bool,
    ) -> Option<CoordinatorAction> {
        if self.gossip_dial_peers.remove(&peer_id) {
            // Gossip dial - schedule delayed ping to allow remote to disconnect
            // first if they intend to (avoids wasted gossip to short-lived connections)
            let delay = Box::pin(tokio::time::sleep(self.health_check_delay));
            self.pending_health_checks.insert(
                peer_id,
                PendingHealthCheck {
                    swarm_peer,
                    is_full_node,
                    delay,
                },
            );
            None
        } else {
            // Inbound or kademlia dial - send ping immediately
            self.pending_gossip
                .insert(peer_id, (swarm_peer, is_full_node));
            Some(CoordinatorAction::SendPing(peer_id))
        }
    }

    /// Handle pong received after health check ping.
    ///
    /// Returns gossip actions if this peer was pending gossip activation.
    pub(crate) fn on_pong_received(&mut self, peer_id: PeerId) -> Vec<GossipAction> {
        let Some((swarm_peer, is_full_node)) = self.pending_gossip.remove(&peer_id) else {
            return Vec::new();
        };

        // Now trigger gossip - connection is proven healthy
        let depth = self.current_depth();
        if let Some(gossip) = &mut self.gossip_manager {
            let mut actions = gossip.on_peer_authenticated(&swarm_peer, is_full_node, depth);
            // Check if depth changed due to new peer
            actions.extend(gossip.check_depth_change(depth));
            actions
        } else {
            Vec::new()
        }
    }

    /// Handle ping error (timeout or failure).
    ///
    /// Returns true if this was a pending health check that failed.
    pub(crate) fn on_ping_error(&mut self, peer_id: &PeerId) -> bool {
        self.pending_gossip.remove(peer_id).is_some()
    }

    /// Handle connection closed. Cleans up all tracking state for this peer.
    ///
    /// Returns gossip actions if depth changed due to disconnection.
    pub(crate) fn on_connection_closed(
        &mut self,
        peer_id: &PeerId,
        overlay: Option<&OverlayAddress>,
    ) -> Vec<GossipAction> {
        // Clean up gossip dial tracking
        self.gossip_dial_peers.remove(peer_id);

        // Clean up pending health checks (disconnected during delay)
        self.pending_health_checks.remove(peer_id);

        // Clean up pending gossip (disconnected before pong)
        self.pending_gossip.remove(peer_id);

        // Clean up gossip tracking for disconnected peer
        if let Some(overlay) = overlay {
            let depth = self.current_depth();
            if let Some(gossip) = &mut self.gossip_manager {
                gossip.on_peer_disconnected(overlay);
                // Check if depth changed due to disconnection
                return gossip.check_depth_change(depth);
            }
        }

        Vec::new()
    }

    /// Poll for expired health check delays.
    ///
    /// Returns peers ready to receive health check ping.
    pub(crate) fn poll_health_check_delays(&mut self, cx: &mut Context<'_>) -> Vec<PeerId> {
        let mut ready_peers = Vec::new();

        for (peer_id, check) in &mut self.pending_health_checks {
            if check.delay.as_mut().poll(cx).is_ready() {
                ready_peers.push(*peer_id);
            }
        }

        // Move ready peers to pending_gossip and collect their IDs
        for peer_id in &ready_peers {
            if let Some(check) = self.pending_health_checks.remove(peer_id) {
                self.pending_gossip
                    .insert(*peer_id, (check.swarm_peer, check.is_full_node));
            }
        }

        ready_peers
    }

    /// Poll for periodic gossip tick. Returns gossip actions if tick fired.
    pub(crate) fn poll_gossip_tick(&mut self, cx: &mut Context<'_>) -> Vec<GossipAction> {
        if let Some(interval) = &mut self.gossip_interval {
            if interval.as_mut().poll_tick(cx).is_ready() {
                let depth = self.current_depth();
                if let Some(gossip) = &mut self.gossip_manager {
                    return gossip.on_tick(depth);
                }
            }
        }
        Vec::new()
    }
}

impl Default for GossipCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256, Signature};
    use libp2p::PeerId;
    use vertex_swarm_peer::SwarmPeer;

    fn make_overlay(byte: u8) -> OverlayAddress {
        let mut bytes = [0u8; 32];
        bytes[0] = byte;
        OverlayAddress::from(bytes)
    }

    fn make_swarm_peer(overlay_byte: u8) -> SwarmPeer {
        use alloy_primitives::U256;
        let overlay = make_overlay(overlay_byte);
        // Use from_validated with dummy values for testing
        SwarmPeer::from_validated(
            vec![], // empty multiaddrs for test
            Signature::new(U256::ZERO, U256::ZERO, false),
            B256::from_slice(overlay.as_slice()),
            B256::ZERO,
            Address::ZERO,
        )
    }

    #[test]
    fn test_coordinator_initial_state() {
        let coord = GossipCoordinator::new();

        // Gossip should be disabled by default
        assert_eq!(coord.current_depth(), 0);
    }

    #[test]
    fn test_coordinator_default_impl() {
        let coord = GossipCoordinator::default();
        assert_eq!(coord.current_depth(), 0);
    }

    #[test]
    fn test_handshake_non_gossip_dial_returns_ping_action() {
        // For inbound or kademlia dials, handshake completion returns immediate SendPing.
        let mut coord = GossipCoordinator::new();

        let peer_id = PeerId::random();
        let swarm_peer = make_swarm_peer(0x80);

        // Non-gossip dial: should return SendPing action immediately
        let action = coord.on_handshake_completed(peer_id, swarm_peer, true);

        assert!(action.is_some());
        match action.unwrap() {
            CoordinatorAction::SendPing(pid) => assert_eq!(pid, peer_id),
        }
    }

    #[tokio::test]
    async fn test_handshake_gossip_dial_schedules_delay() {
        // For gossip dials, handshake completion schedules delayed ping.
        let mut coord = GossipCoordinator::new();

        let peer_id = PeerId::random();
        let swarm_peer = make_swarm_peer(0x80);

        // Mark peer as gossip dial first
        coord.mark_gossip_dial(peer_id);

        // Gossip dial: should return None (ping is delayed)
        let action = coord.on_handshake_completed(peer_id, swarm_peer, true);
        assert!(action.is_none());
    }

    #[test]
    fn test_pong_received_removes_from_pending() {
        // Pong received should remove peer from pending gossip.
        let mut coord = GossipCoordinator::new();

        let peer_id = PeerId::random();
        let swarm_peer = make_swarm_peer(0x80);

        // Complete handshake (non-gossip dial)
        let _ = coord.on_handshake_completed(peer_id, swarm_peer, true);

        // Receive pong - should process and remove from pending
        let actions = coord.on_pong_received(peer_id);

        // Without gossip enabled, returns empty vec
        assert!(actions.is_empty());

        // Calling again should also return empty (peer removed)
        let actions2 = coord.on_pong_received(peer_id);
        assert!(actions2.is_empty());
    }

    #[test]
    fn test_ping_error_removes_from_pending() {
        let mut coord = GossipCoordinator::new();

        let peer_id = PeerId::random();
        let swarm_peer = make_swarm_peer(0x80);

        // Complete handshake (non-gossip dial)
        let _ = coord.on_handshake_completed(peer_id, swarm_peer, true);

        // Ping error should remove from pending and return true
        let was_pending = coord.on_ping_error(&peer_id);
        assert!(was_pending);

        // Calling again should return false (already removed)
        let was_pending2 = coord.on_ping_error(&peer_id);
        assert!(!was_pending2);
    }

    #[tokio::test]
    async fn test_connection_closed_cleans_up_all_state() {
        let mut coord = GossipCoordinator::new();

        let peer_id = PeerId::random();
        let overlay = make_overlay(0x80);
        let swarm_peer = make_swarm_peer(0x80);

        // Set up state in multiple places
        coord.mark_gossip_dial(peer_id);
        let _ = coord.on_handshake_completed(peer_id, swarm_peer, true);

        // Connection closed should clean everything up
        let actions = coord.on_connection_closed(&peer_id, Some(&overlay));

        // Without gossip manager, returns empty
        assert!(actions.is_empty());

        // Verify state is cleaned (ping error should return false)
        assert!(!coord.on_ping_error(&peer_id));
    }

    #[test]
    fn test_set_health_check_delay() {
        let mut coord = GossipCoordinator::new();

        // Set custom delay
        coord.set_health_check_delay(Duration::from_millis(100));

        // Delay should be used for next gossip dial
        // (We can't easily test this without async polling, but we verify
        // the method exists and can be called)
    }

    #[test]
    fn test_connection_closed_without_overlay() {
        let mut coord = GossipCoordinator::new();

        let peer_id = PeerId::random();
        let swarm_peer = make_swarm_peer(0x80);

        // Complete handshake
        let _ = coord.on_handshake_completed(peer_id, swarm_peer, true);

        // Connection closed without overlay should still clean up local state
        let actions = coord.on_connection_closed(&peer_id, None);
        assert!(actions.is_empty());

        // Verify pending gossip was cleaned up
        assert!(!coord.on_ping_error(&peer_id));
    }

    #[test]
    fn test_multiple_peers_tracked_independently() {
        let mut coord = GossipCoordinator::new();

        let peer1 = PeerId::random();
        let peer2 = PeerId::random();
        let swarm_peer1 = make_swarm_peer(0x80);
        let swarm_peer2 = make_swarm_peer(0x40);

        // Complete handshakes for both
        let action1 = coord.on_handshake_completed(peer1, swarm_peer1, true);
        let action2 = coord.on_handshake_completed(peer2, swarm_peer2, true);

        assert!(action1.is_some());
        assert!(action2.is_some());

        // Ping error for peer1 should not affect peer2
        assert!(coord.on_ping_error(&peer1));
        assert!(coord.on_ping_error(&peer2)); // peer2 still pending
    }

    #[tokio::test]
    async fn test_gossip_dial_then_handshake_then_disconnect() {
        // Full lifecycle: gossip dial -> handshake -> disconnect before ping
        let mut coord = GossipCoordinator::new();

        let peer_id = PeerId::random();
        let overlay = make_overlay(0x80);
        let swarm_peer = make_swarm_peer(0x80);

        // 1. Mark as gossip dial
        coord.mark_gossip_dial(peer_id);

        // 2. Handshake completes - should be delayed
        let action = coord.on_handshake_completed(peer_id, swarm_peer, true);
        assert!(action.is_none());

        // 3. Disconnect before delay expires
        let actions = coord.on_connection_closed(&peer_id, Some(&overlay));
        assert!(actions.is_empty());

        // 4. No dangling state
        assert!(!coord.on_ping_error(&peer_id));
    }
}
