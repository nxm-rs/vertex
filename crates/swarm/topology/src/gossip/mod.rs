//! Hive gossip coordination for peer discovery.
//!
//! Manages the health check and gossip lifecycle:
//! connection → handshake → health check delay → ping → pong → gossip.

pub(crate) mod verifier;
pub(crate) mod verifier_task;

use std::{
    collections::{HashMap, HashSet},
    pin::Pin,
    sync::Arc,
    task::Context,
    time::{Duration, Instant},
};

use libp2p::PeerId;
use tokio::time::{Interval, Sleep};
use tracing::{debug, trace};
use vertex_net_local::IpCapability;
use vertex_swarm_peer::{AddressScope, SwarmPeer};
use vertex_swarm_peer_manager::PeerManager;
use vertex_swarm_peer_registry::SwarmPeerRegistry as ConnectionRegistry;
use vertex_swarm_primitives::{OverlayAddress, SwarmNodeType};

/// Interval for refreshing neighborhood peers.
const GOSSIP_REFRESH_INTERVAL: Duration = Duration::from_secs(600);

/// Default delay before sending health check ping after handshake.
const DEFAULT_HEALTH_CHECK_DELAY: Duration = Duration::from_millis(500);

/// Maximum peers to send to distant (non-neighbor) peers.
const MAX_PEERS_FOR_DISTANT: usize = 16;

/// Number of peers close to recipient's overlay to include.
const CLOSE_PEERS_COUNT: usize = 4;

/// An action to send peers to a specific overlay address.
#[derive(Debug, Clone)]
pub(crate) struct GossipAction {
    pub to: OverlayAddress,
    pub peers: Vec<SwarmPeer>,
}

/// Action returned by gossip for TopologyBehaviour to execute.
#[derive(Debug)]
pub(crate) enum GossipCommand {
    SendPing(PeerId),
}

struct PendingHealthCheck {
    swarm_peer: SwarmPeer,
    node_type: SwarmNodeType,
    delay: Pin<Box<Sleep>>,
}

/// Hive gossip manager coordinating peer discovery and health checks.
pub(crate) struct Gossip {
    local_overlay: OverlayAddress,
    peer_manager: Arc<PeerManager>,
    connection_registry: Arc<ConnectionRegistry>,
    current_depth: u8,
    last_depth: u8,
    last_broadcast: HashMap<OverlayAddress, Instant>,
    gossip_dial_peers: HashSet<PeerId>,
    pending_health_checks: HashMap<PeerId, PendingHealthCheck>,
    pending_gossip: HashMap<PeerId, (SwarmPeer, SwarmNodeType)>,
    health_check_delay: Duration,
    gossip_interval: Pin<Box<Interval>>,
}

impl Gossip {
    pub(crate) fn new(
        local_overlay: OverlayAddress,
        peer_manager: Arc<PeerManager>,
        connection_registry: Arc<ConnectionRegistry>,
    ) -> Self {
        Self {
            local_overlay,
            peer_manager,
            connection_registry,
            current_depth: 0,
            last_depth: 0,
            last_broadcast: HashMap::new(),
            gossip_dial_peers: HashSet::new(),
            pending_health_checks: HashMap::new(),
            pending_gossip: HashMap::new(),
            health_check_delay: DEFAULT_HEALTH_CHECK_DELAY,
            gossip_interval: Box::pin(tokio::time::interval(GOSSIP_REFRESH_INTERVAL)),
        }
    }

    pub(crate) fn set_depth(&mut self, depth: u8) {
        self.current_depth = depth;
    }

    pub(crate) fn mark_gossip_dial(&mut self, peer_id: PeerId) {
        self.gossip_dial_peers.insert(peer_id);
    }

    /// Handle handshake completion.
    ///
    /// For gossip dials: schedules delayed ping, returns None.
    /// For inbound/kademlia dials: returns SendPing command for immediate health check.
    pub(crate) fn on_handshake_completed(
        &mut self,
        peer_id: PeerId,
        swarm_peer: SwarmPeer,
        node_type: SwarmNodeType,
    ) -> Option<GossipCommand> {
        if self.gossip_dial_peers.remove(&peer_id) {
            let delay = Box::pin(tokio::time::sleep(self.health_check_delay));
            self.pending_health_checks.insert(
                peer_id,
                PendingHealthCheck {
                    swarm_peer,
                    node_type,
                    delay,
                },
            );
            None
        } else {
            self.pending_gossip
                .insert(peer_id, (swarm_peer, node_type));
            Some(GossipCommand::SendPing(peer_id))
        }
    }

    pub(crate) fn on_pong_received(&mut self, peer_id: PeerId) -> Vec<GossipAction> {
        let Some((swarm_peer, node_type)) = self.pending_gossip.remove(&peer_id) else {
            return Vec::new();
        };

        let depth = self.current_depth;
        let mut actions = self.on_peer_authenticated(&swarm_peer, node_type, depth);
        actions.extend(self.check_depth_change());
        actions
    }

    pub(crate) fn on_ping_error(&mut self, peer_id: &PeerId) -> bool {
        self.pending_gossip.remove(peer_id).is_some()
    }

    pub(crate) fn on_connection_closed(
        &mut self,
        peer_id: &PeerId,
        overlay: Option<&OverlayAddress>,
    ) -> Vec<GossipAction> {
        self.gossip_dial_peers.remove(peer_id);
        self.pending_health_checks.remove(peer_id);
        self.pending_gossip.remove(peer_id);

        if let Some(overlay) = overlay {
            self.last_broadcast.remove(overlay);
            return self.check_depth_change();
        }

        Vec::new()
    }

    pub(crate) fn poll_health_check_delays(&mut self, cx: &mut Context<'_>) -> Vec<PeerId> {
        let mut ready_peers = Vec::new();

        for (peer_id, check) in &mut self.pending_health_checks {
            if check.delay.as_mut().poll(cx).is_ready() {
                ready_peers.push(*peer_id);
            }
        }

        for peer_id in &ready_peers {
            if let Some(check) = self.pending_health_checks.remove(peer_id) {
                self.pending_gossip
                    .insert(*peer_id, (check.swarm_peer, check.node_type));
            }
        }

        ready_peers
    }

    pub(crate) fn poll_tick(&mut self, cx: &mut Context<'_>) -> Vec<GossipAction> {
        if self.gossip_interval.as_mut().poll_tick(cx).is_ready() {
            return self.on_tick();
        }
        Vec::new()
    }

    fn check_depth_change(&mut self) -> Vec<GossipAction> {
        let current = self.current_depth;
        if current == self.last_depth {
            return Vec::new();
        }

        let old_depth = self.last_depth;
        self.last_depth = current;

        if current >= old_depth {
            return Vec::new();
        }

        debug!(old_depth, new_depth = current, "Depth decreased - neighborhood expanded");

        let mut actions = Vec::new();

        // Find CONNECTED peers that are now neighbors but weren't before
        for overlay in self.connection_registry.active_peers() {
            let proximity = self.local_overlay.proximity(&overlay);

            if proximity >= current
                && proximity < old_depth
                && self.peer_manager.node_type(&overlay) == Some(SwarmNodeType::Storer)
            {
                debug!(%overlay, proximity, "Peer became neighbor due to depth change");

                if let Some(peer) = self.peer_manager.get_swarm_peer(&overlay) {
                    actions.extend(self.handle_new_neighbor(overlay, peer, current));
                }
            }
        }

        actions
    }

    fn on_peer_authenticated(
        &mut self,
        peer: &SwarmPeer,
        node_type: SwarmNodeType,
        depth: u8,
    ) -> Vec<GossipAction> {
        self.last_depth = depth;

        if !node_type.requires_storage() {
            trace!(overlay = %peer.overlay(), "Skipping gossip for non-storer node");
            return Vec::new();
        }

        let new_peer_overlay = OverlayAddress::from(*peer.overlay());
        let proximity = self.local_overlay.proximity(&new_peer_overlay);

        if proximity >= depth {
            self.handle_new_neighbor(new_peer_overlay, peer.clone(), depth)
        } else {
            self.handle_new_distant_peer(new_peer_overlay)
        }
    }

    fn on_tick(&mut self) -> Vec<GossipAction> {
        let now = Instant::now();
        let mut actions = Vec::new();
        let depth = self.current_depth;

        // Only send to CONNECTED neighbors
        let neighbors = self.get_connected_neighbors(depth);

        for neighbor in neighbors {
            let is_stale = self
                .last_broadcast
                .get(&neighbor)
                .map(|t| now.duration_since(*t) > GOSSIP_REFRESH_INTERVAL)
                .unwrap_or(true);

            if is_stale {
                let neighbor_capability = self.get_peer_capability(&neighbor);
                let neighbor_scope = self.get_peer_scope(&neighbor);
                // Content can include KNOWN peers (not just connected)
                let peers = self.get_known_neighborhood_peers(depth, Some(&neighbor));
                let filtered_peers =
                    self.filter_peers_for_recipient(&peers, neighbor_capability, neighbor_scope);

                if !filtered_peers.is_empty() {
                    trace!(to = %neighbor, count = filtered_peers.len(), "Refreshing neighborhood peers");
                    actions.push(GossipAction {
                        to: neighbor,
                        peers: filtered_peers,
                    });
                    self.last_broadcast.insert(neighbor, now);
                }
            }
        }

        actions
    }

    fn handle_new_neighbor(
        &mut self,
        new_peer: OverlayAddress,
        new_peer_info: SwarmPeer,
        depth: u8,
    ) -> Vec<GossipAction> {
        let mut actions = Vec::new();

        debug!(%new_peer, depth, "New neighbor joined - initiating neighborhood exchange");

        let new_peer_capability = self.get_peer_capability(&new_peer);
        let new_peer_scope = self.get_peer_scope(&new_peer);

        // Send new peer all KNOWN neighborhood peers (content)
        let neighborhood_peers = self.get_known_neighborhood_peers(depth, Some(&new_peer));
        let filtered_peers =
            self.filter_peers_for_recipient(&neighborhood_peers, new_peer_capability, new_peer_scope);

        if !filtered_peers.is_empty() {
            debug!(to = %new_peer, count = filtered_peers.len(), "Sending known neighborhood peers");
            actions.push(GossipAction {
                to: new_peer,
                peers: filtered_peers,
            });
        }

        // Notify CONNECTED neighbors about new peer (recipients must be connected)
        let existing_neighbors = self.get_connected_neighbors(depth);
        for neighbor in existing_neighbors {
            if neighbor != new_peer {
                let neighbor_capability = self.get_peer_capability(&neighbor);
                let neighbor_scope = self.get_peer_scope(&neighbor);

                // Filter the new peer for this neighbor (checks capability + scope)
                let filtered =
                    self.filter_peers_for_recipient(&[new_peer_info.clone()], neighbor_capability, neighbor_scope);

                if !filtered.is_empty() {
                    trace!(to = %neighbor, about = %new_peer, "Notifying neighbor about new peer");
                    actions.push(GossipAction {
                        to: neighbor,
                        peers: filtered,
                    });
                }
            }
        }

        self.last_broadcast.insert(new_peer, Instant::now());
        actions
    }

    fn handle_new_distant_peer(&mut self, peer: OverlayAddress) -> Vec<GossipAction> {
        let recipient_capability = self.get_peer_capability(&peer);
        let recipient_scope = self.get_peer_scope(&peer);
        // Select from KNOWN peers to send to this distant peer
        let peers = self.select_peers_for_distant(peer, recipient_capability, recipient_scope);

        if peers.is_empty() {
            return Vec::new();
        }

        debug!(to = %peer, count = peers.len(), "Sending bootstrap peers to distant peer");

        self.last_broadcast.insert(peer, Instant::now());
        vec![GossipAction { to: peer, peers }]
    }

    /// Get CONNECTED full-node neighbors (proximity >= depth).
    /// Used for determining WHO to send gossip to.
    fn get_connected_neighbors(&self, depth: u8) -> Vec<OverlayAddress> {
        self.connection_registry
            .active_peers()
            .into_iter()
            .filter(|overlay| {
                self.local_overlay.proximity(overlay) >= depth
                    && self.peer_manager.node_type(overlay) == Some(SwarmNodeType::Storer)
            })
            .collect()
    }

    /// Get SwarmPeer data for KNOWN neighborhood peers.
    /// Used for determining WHAT peers to share (content).
    fn get_known_neighborhood_peers(
        &self,
        depth: u8,
        exclude: Option<&OverlayAddress>,
    ) -> Vec<SwarmPeer> {
        let overlays: Vec<_> = self
            .peer_manager
            .known_storer_overlays()
            .into_iter()
            .filter(|overlay| {
                if exclude.map(|e| overlay == e).unwrap_or(false) {
                    return false;
                }
                self.local_overlay.proximity(overlay) >= depth
            })
            .collect();

        self.peer_manager.get_swarm_peers(&overlays)
    }

    /// Select KNOWN peers to send to a distant peer (bootstrap help).
    #[tracing::instrument(skip(self), level = "trace", fields(%recipient))]
    fn select_peers_for_distant(
        &self,
        recipient: OverlayAddress,
        recipient_capability: IpCapability,
        recipient_scope: AddressScope,
    ) -> Vec<SwarmPeer> {
        let mut selected = Vec::with_capacity(MAX_PEERS_FOR_DISTANT);
        let mut selected_indices: HashSet<usize> = HashSet::with_capacity(MAX_PEERS_FOR_DISTANT);
        let mut added_bins: HashSet<u8> = HashSet::new();

        let all_storers = self.peer_manager.known_storer_overlays();

        // Build candidate list with index-based tracking (avoids clone for sorting)
        let storers: Vec<_> = all_storers
            .iter()
            .filter_map(|overlay| {
                let capability = self.get_peer_capability(overlay);
                if !Self::capabilities_compatible(recipient_capability, capability) {
                    return None;
                }

                let peer = self.peer_manager.get_swarm_peer(overlay)?;

                // Strict scope check for public recipients
                if recipient_scope == AddressScope::Public {
                    let has_non_public = peer.has_scope(AddressScope::Private)
                        || peer.has_scope(AddressScope::LinkLocal)
                        || peer.has_scope(AddressScope::Loopback);
                    if has_non_public || !peer.has_scope(AddressScope::Public) {
                        return None;
                    }
                }

                let proximity_to_recipient = recipient.proximity(overlay);
                let bin = self.local_overlay.proximity(overlay);
                Some((peer, proximity_to_recipient, bin))
            })
            .collect();

        if storers.is_empty() {
            return selected;
        }

        // Phase 1: Top CLOSE_PEERS_COUNT by proximity to recipient (sort indices, not data)
        let mut indices: Vec<usize> = (0..storers.len()).collect();
        indices.sort_by(|&a, &b| storers[b].1.cmp(&storers[a].1));

        for &idx in indices.iter().take(CLOSE_PEERS_COUNT) {
            if selected_indices.insert(idx) {
                selected.push(storers[idx].0.clone());
            }
        }

        // Phase 2: One peer per bin (routing diversity)
        for (idx, (peer, _, bin)) in storers.iter().enumerate() {
            if selected.len() >= MAX_PEERS_FOR_DISTANT {
                break;
            }
            if !selected_indices.contains(&idx) && added_bins.insert(*bin) {
                selected_indices.insert(idx);
                selected.push(peer.clone());
            }
        }

        // Phase 3: Fill remaining slots
        for (idx, (peer, _, _)) in storers.iter().enumerate() {
            if selected.len() >= MAX_PEERS_FOR_DISTANT {
                break;
            }
            if selected_indices.insert(idx) {
                selected.push(peer.clone());
            }
        }

        selected
    }

    fn get_peer_capability(&self, overlay: &OverlayAddress) -> IpCapability {
        self.peer_manager
            .get_peer_capability(overlay)
            .unwrap_or_else(IpCapability::dual_stack)
    }

    fn get_peer_scope(&self, overlay: &OverlayAddress) -> AddressScope {
        self.peer_manager
            .get_swarm_peer(overlay)
            .and_then(|p| p.max_scope())
            .unwrap_or(AddressScope::Public)
    }

    /// Filter peers for gossiping to a recipient.
    ///
    /// For public recipients: excludes peers with ANY private/loopback addresses entirely.
    /// SwarmPeer is cryptographically signed over all multiaddrs, so we cannot filter
    /// individual addresses - the entire peer must be excluded if it contains private IPs.
    fn filter_peers_for_recipient(
        &self,
        peers: &[SwarmPeer],
        recipient_capability: IpCapability,
        recipient_scope: AddressScope,
    ) -> Vec<SwarmPeer> {
        peers
            .iter()
            .filter(|peer| {
                // IP capability check
                let peer_overlay = OverlayAddress::from(*peer.overlay());
                let peer_capability = self.get_peer_capability(&peer_overlay);
                if !Self::capabilities_compatible(recipient_capability, peer_capability) {
                    return false;
                }

                // Strict scope check for public recipients
                if recipient_scope == AddressScope::Public {
                    // If peer has ANY private/loopback addresses, exclude entirely
                    // (signed SwarmPeer can't have addresses removed)
                    let has_non_public = peer.has_scope(AddressScope::Private)
                        || peer.has_scope(AddressScope::LinkLocal)
                        || peer.has_scope(AddressScope::Loopback);

                    // Only include if ONLY public addresses
                    !has_non_public && peer.has_scope(AddressScope::Public)
                } else {
                    // Private/loopback recipient - any addresses ok
                    true
                }
            })
            .cloned()
            .collect()
    }

    fn capabilities_compatible(recipient: IpCapability, peer: IpCapability) -> bool {
        if recipient.is_empty() {
            return false;
        }
        (recipient.supports_ipv4() && peer.supports_ipv4())
            || (recipient.supports_ipv6() && peer.supports_ipv6())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_primitives::SwarmNodeType;
    use vertex_swarm_test_utils::{test_overlay, test_swarm_peer};

    fn make_gossip() -> Gossip {
        let local = test_overlay(0);
        let pm = PeerManager::new(local, 31);
        let cr = Arc::new(ConnectionRegistry::new());
        Gossip::new(local, pm, cr)
    }

    fn make_gossip_with_peers() -> Gossip {
        let local = test_overlay(0);
        let pm = PeerManager::new(local, 31);
        let cr = Arc::new(ConnectionRegistry::new());

        for n in 1..=10 {
            pm.on_peer_ready(test_swarm_peer(n), SwarmNodeType::Storer);
        }

        Gossip::new(local, pm, cr)
    }

    #[tokio::test]
    async fn test_initial_state() {
        let gossip = make_gossip();
        assert_eq!(gossip.current_depth, 0);
    }

    #[tokio::test]
    async fn test_set_depth() {
        let mut gossip = make_gossip();
        gossip.set_depth(8);
        assert_eq!(gossip.current_depth, 8);
    }

    #[tokio::test]
    async fn test_handshake_non_gossip_dial_returns_ping() {
        let mut gossip = make_gossip();

        let peer_id = PeerId::random();
        let swarm_peer = test_swarm_peer(0x80);

        let cmd = gossip.on_handshake_completed(peer_id, swarm_peer, SwarmNodeType::Storer);

        assert!(cmd.is_some());
        match cmd.unwrap() {
            GossipCommand::SendPing(pid) => assert_eq!(pid, peer_id),
        }
    }

    #[tokio::test]
    async fn test_handshake_gossip_dial_schedules_delay() {
        let mut gossip = make_gossip();

        let peer_id = PeerId::random();
        let swarm_peer = test_swarm_peer(0x80);

        gossip.mark_gossip_dial(peer_id);
        let cmd = gossip.on_handshake_completed(peer_id, swarm_peer, SwarmNodeType::Storer);
        assert!(cmd.is_none());
    }

    #[tokio::test]
    async fn test_pong_received_removes_from_pending() {
        let mut gossip = make_gossip();

        let peer_id = PeerId::random();
        let swarm_peer = test_swarm_peer(0x80);

        gossip.on_handshake_completed(peer_id, swarm_peer, SwarmNodeType::Storer);
        gossip.on_pong_received(peer_id);

        let actions = gossip.on_pong_received(peer_id);
        assert!(actions.is_empty());
    }

    #[tokio::test]
    async fn test_ping_error_removes_from_pending() {
        let mut gossip = make_gossip();

        let peer_id = PeerId::random();
        let swarm_peer = test_swarm_peer(0x80);

        gossip.on_handshake_completed(peer_id, swarm_peer, SwarmNodeType::Storer);

        assert!(gossip.on_ping_error(&peer_id));
        assert!(!gossip.on_ping_error(&peer_id));
    }

    #[tokio::test]
    async fn test_connection_closed_cleans_up() {
        let mut gossip = make_gossip();

        let peer_id = PeerId::random();
        let overlay = test_overlay(0x80);
        let swarm_peer = test_swarm_peer(0x80);

        gossip.mark_gossip_dial(peer_id);
        gossip.on_handshake_completed(peer_id, swarm_peer, SwarmNodeType::Storer);

        gossip.on_connection_closed(&peer_id, Some(&overlay));
        assert!(!gossip.on_ping_error(&peer_id));
    }

    #[tokio::test]
    async fn test_get_connected_neighbors_empty_when_no_connections() {
        let gossip = make_gossip_with_peers();
        // No connections registered, so should return empty
        let neighbors = gossip.get_connected_neighbors(0);
        assert!(neighbors.is_empty());
    }

    #[tokio::test]
    async fn test_on_tick_no_actions_when_no_connections() {
        let mut gossip = make_gossip_with_peers();
        // No connections, so on_tick should produce no actions
        let actions = gossip.on_tick();
        assert!(actions.is_empty());
    }

    #[tokio::test]
    async fn test_select_peers_no_duplicates() {
        let gossip = make_gossip_with_peers();
        let recipient = test_overlay(0xFF);
        let capability = IpCapability::dual_stack();
        // Use Loopback scope since test peers have loopback addresses
        let scope = AddressScope::Loopback;

        let selected = gossip.select_peers_for_distant(recipient, capability, scope);

        let unique: HashSet<_> = selected.iter().map(|p| *p.overlay()).collect();
        assert_eq!(unique.len(), selected.len());
    }

    #[tokio::test]
    async fn test_check_depth_change_no_change() {
        let mut gossip = make_gossip_with_peers();
        gossip.last_depth = 5;
        gossip.current_depth = 5;

        let actions = gossip.check_depth_change();
        assert!(actions.is_empty());
    }

    #[tokio::test]
    async fn test_filter_peers_dual_stack() {
        let gossip = make_gossip_with_peers();
        let peers = vec![test_swarm_peer(1), test_swarm_peer(2)];

        // Use Loopback scope since test peers have loopback addresses
        let filtered =
            gossip.filter_peers_for_recipient(&peers, IpCapability::dual_stack(), AddressScope::Loopback);
        assert_eq!(filtered.len(), 2);
    }

    #[tokio::test]
    async fn test_filter_peers_public_recipient_excludes_loopback() {
        let gossip = make_gossip_with_peers();
        // test_swarm_peer creates peers with loopback addresses (127.0.0.1)
        let peers = vec![test_swarm_peer(1), test_swarm_peer(2)];

        // Public recipient should exclude peers with ANY non-public addresses
        let filtered =
            gossip.filter_peers_for_recipient(&peers, IpCapability::dual_stack(), AddressScope::Public);
        assert!(filtered.is_empty(), "Loopback peers should be excluded for public recipients");
    }
}
