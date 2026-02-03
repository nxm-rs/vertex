//! Hive gossip strategy for peer discovery.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use tracing::{debug, trace};
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_peermanager::{IpCapability, PeerManager};
use vertex_swarm_primitives::OverlayAddress;

/// Configuration for hive gossip behavior.
#[derive(Debug, Clone)]
pub struct HiveGossipConfig {
    /// Interval for refreshing neighborhood peers.
    pub refresh_interval: Duration,
    /// Maximum peers to send to distant (non-neighbor) peers.
    pub max_peers_for_distant: usize,
    /// Number of peers close to recipient's overlay to include.
    pub close_peers_count: usize,
}

impl Default for HiveGossipConfig {
    fn default() -> Self {
        Self {
            refresh_interval: Duration::from_secs(600), // 10 minutes
            max_peers_for_distant: 16,
            close_peers_count: 4,
        }
    }
}

impl HiveGossipConfig {
    /// Set the refresh interval.
    pub fn with_refresh_interval(mut self, interval: Duration) -> Self {
        self.refresh_interval = interval;
        self
    }

    /// Set the maximum peers for distant nodes.
    pub fn with_max_peers_for_distant(mut self, count: usize) -> Self {
        self.max_peers_for_distant = count;
        self
    }
}

/// An action to send peers to a specific overlay address.
#[derive(Debug, Clone)]
pub(crate) struct GossipAction {
    /// The recipient of the gossip.
    pub to: OverlayAddress,
    /// The peers to send.
    pub peers: Vec<SwarmPeer>,
}

/// Hive gossip manager.
pub(crate) struct HiveGossipManager {
    config: HiveGossipConfig,
    local_overlay: OverlayAddress,
    peer_manager: Arc<PeerManager>,
    last_broadcast: HashMap<OverlayAddress, Instant>,
    last_depth: u8,
    last_tick: Instant,
}

impl HiveGossipManager {
    /// Create a new gossip manager.
    pub(crate) fn new(
        config: HiveGossipConfig,
        local_overlay: OverlayAddress,
        peer_manager: Arc<PeerManager>,
    ) -> Self {
        Self {
            config,
            local_overlay,
            peer_manager,
            last_broadcast: HashMap::new(),
            last_depth: 0,
            last_tick: Instant::now(),
        }
    }

    /// Check if depth has changed and return any resulting gossip actions.
    pub(crate) fn check_depth_change(&mut self, current_depth: u8) -> Vec<GossipAction> {
        if current_depth == self.last_depth {
            return Vec::new();
        }

        let old_depth = self.last_depth;
        self.last_depth = current_depth;

        self.on_depth_changed(old_depth, current_depth)
    }

    /// Handle a new peer authentication. Returns gossip actions for full nodes only.
    pub(crate) fn on_peer_authenticated(
        &mut self,
        peer: &SwarmPeer,
        is_full_node: bool,
        depth: u8,
    ) -> Vec<GossipAction> {
        // Update our depth tracking
        self.last_depth = depth;

        if !is_full_node {
            // Light nodes receive nothing and are never gossiped about
            trace!(overlay = %peer.overlay(), "Skipping gossip for light node");
            return Vec::new();
        }

        let new_peer_overlay = OverlayAddress::from(*peer.overlay());
        let proximity = self.local_overlay.proximity(&new_peer_overlay);

        if proximity >= depth {
            // New peer is a neighbor - critical path for replication
            self.handle_new_neighbor(new_peer_overlay, peer.clone(), depth)
        } else {
            // Distant peer - help them bootstrap
            self.handle_new_distant_peer(new_peer_overlay)
        }
    }

    /// Handle a new neighbor joining our neighborhood.
    fn handle_new_neighbor(
        &mut self,
        new_peer: OverlayAddress,
        new_peer_info: SwarmPeer,
        depth: u8,
    ) -> Vec<GossipAction> {
        let mut actions = Vec::new();

        debug!(%new_peer, depth, "New neighbor joined - initiating full neighborhood exchange");

        // Get new peer's IP capability for filtering
        let new_peer_capability = self.get_peer_capability(&new_peer);

        // 1. Send new peer all current neighborhood peers (filtered for their capability)
        let neighborhood_peers = self.get_neighborhood_peers(depth, Some(&new_peer));
        let filtered_peers =
            self.filter_peers_for_recipient(neighborhood_peers, new_peer_capability);
        if !filtered_peers.is_empty() {
            debug!(
                to = %new_peer,
                count = filtered_peers.len(),
                ?new_peer_capability,
                "Sending neighborhood peers to new neighbor"
            );
            actions.push(GossipAction {
                to: new_peer,
                peers: filtered_peers,
            });
        }

        // 2. Notify all existing neighbors about the new peer (if they can reach it)
        let existing_neighbors = self.get_connected_neighbors(depth);
        for neighbor in existing_neighbors {
            if neighbor != new_peer {
                // Check if the existing neighbor can reach the new peer
                let neighbor_capability = self.get_peer_capability(&neighbor);
                let reachable_peers = self
                    .filter_peers_for_recipient(vec![new_peer_info.clone()], neighbor_capability);

                if !reachable_peers.is_empty() {
                    trace!(
                        to = %neighbor,
                        about = %new_peer,
                        ?neighbor_capability,
                        "Notifying existing neighbor about new peer"
                    );
                    actions.push(GossipAction {
                        to: neighbor,
                        peers: reachable_peers,
                    });
                } else {
                    trace!(
                        to = %neighbor,
                        about = %new_peer,
                        ?neighbor_capability,
                        "Skipping notification - neighbor cannot reach new peer"
                    );
                }
            }
        }

        // Record broadcast time
        let now = Instant::now();
        self.last_broadcast.insert(new_peer, now);

        actions
    }

    /// Handle a new distant (non-neighbor) peer.
    fn handle_new_distant_peer(&mut self, peer: OverlayAddress) -> Vec<GossipAction> {
        let recipient_capability = self.get_peer_capability(&peer);
        let peers = self.select_peers_for_distant(peer, recipient_capability);

        if peers.is_empty() {
            return Vec::new();
        }

        debug!(
            to = %peer,
            count = peers.len(),
            ?recipient_capability,
            "Sending bootstrap peers to distant peer"
        );

        // Record broadcast time
        self.last_broadcast.insert(peer, Instant::now());

        vec![GossipAction { to: peer, peers }]
    }

    /// Handle depth change (neighborhood expansion triggers gossip to new neighbors).
    fn on_depth_changed(&mut self, old_depth: u8, new_depth: u8) -> Vec<GossipAction> {
        if new_depth >= old_depth {
            // Neighborhood shrinking or unchanged - no action needed
            return Vec::new();
        }

        debug!(
            old_depth,
            new_depth, "Depth decreased - neighborhood expanded"
        );

        let mut actions = Vec::new();

        // Find peers that are now neighbors but weren't before
        let connected = self.peer_manager.manager.connected_peers();
        for overlay in connected {
            let proximity = self.local_overlay.proximity(&overlay);

            // This peer is now a neighbor (proximity >= new_depth)
            // but wasn't before (proximity < old_depth)
            if proximity >= new_depth && proximity < old_depth {
                // Check if it's a full node
                if self.peer_manager.is_full_node(&overlay) {
                    debug!(
                        %overlay,
                        proximity,
                        "Peer became neighbor due to depth change"
                    );

                    // Get their SwarmPeer info for notifying others
                    if let Some(snapshot) = self.peer_manager.get_peer_snapshot(&overlay) {
                        if let Some(swarm_peer) = snapshot.ext.peer {
                            let peer_actions =
                                self.handle_new_neighbor(overlay, swarm_peer, new_depth);
                            actions.extend(peer_actions);
                        }
                    }
                }
            }
        }

        actions
    }

    /// Check if it's time for a periodic tick and handle it.
    pub(crate) fn maybe_tick(&mut self, depth: u8) -> Vec<GossipAction> {
        let now = Instant::now();
        if now.duration_since(self.last_tick) < self.config.refresh_interval {
            return Vec::new();
        }
        self.last_tick = now;
        self.on_tick(depth)
    }

    /// Refresh stale neighborhood peers.
    fn on_tick(&mut self, depth: u8) -> Vec<GossipAction> {
        let now = Instant::now();

        let mut actions = Vec::new();

        // Only refresh neighborhood peers
        let neighbors = self.get_connected_neighbors(depth);

        for neighbor in neighbors {
            let is_stale = self
                .last_broadcast
                .get(&neighbor)
                .map(|t| now.duration_since(*t) > self.config.refresh_interval)
                .unwrap_or(true);

            if is_stale {
                // Check if still a full node and connected
                if !self.peer_manager.is_full_node(&neighbor) {
                    continue;
                }

                let proximity = self.local_overlay.proximity(&neighbor);
                if proximity < depth {
                    // No longer a neighbor (depth may have changed)
                    continue;
                }

                // Get and filter peers for this neighbor's IP capability
                let neighbor_capability = self.get_peer_capability(&neighbor);
                let peers = self.get_neighborhood_peers(depth, Some(&neighbor));
                let filtered_peers = self.filter_peers_for_recipient(peers, neighbor_capability);

                if !filtered_peers.is_empty() {
                    trace!(
                        to = %neighbor,
                        count = filtered_peers.len(),
                        ?neighbor_capability,
                        "Refreshing neighborhood peers"
                    );
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

    /// Get all connected full nodes that are neighbors (proximity >= depth).
    fn get_connected_neighbors(&self, depth: u8) -> Vec<OverlayAddress> {
        self.peer_manager
            .manager
            .connected_peers()
            .into_iter()
            .filter(|overlay| {
                let proximity = self.local_overlay.proximity(overlay);
                if proximity < depth {
                    return false;
                }
                // Must be a full node
                self.peer_manager.is_full_node(overlay)
            })
            .collect()
    }

    /// Get SwarmPeer data for all neighborhood full nodes.
    fn get_neighborhood_peers(
        &self,
        depth: u8,
        exclude: Option<&OverlayAddress>,
    ) -> Vec<SwarmPeer> {
        self.peer_manager
            .manager
            .connected_peers()
            .into_iter()
            .filter(|overlay| {
                // Exclude the target peer
                if let Some(excluded) = exclude {
                    if overlay == excluded {
                        return false;
                    }
                }

                // Must be a neighbor
                let proximity = self.local_overlay.proximity(overlay);
                if proximity < depth {
                    return false;
                }

                // Must be a full node
                self.peer_manager.is_full_node(overlay)
            })
            .filter_map(|overlay| {
                self.peer_manager
                    .get_peer_snapshot(&overlay)
                    .and_then(|s| s.ext.peer)
            })
            .collect()
    }

    /// Select peers to send to a distant peer (close peers + diverse sample).
    fn select_peers_for_distant(
        &self,
        recipient: OverlayAddress,
        recipient_capability: IpCapability,
    ) -> Vec<SwarmPeer> {
        let mut selected = Vec::with_capacity(self.config.max_peers_for_distant);

        // Get all connected full nodes with their SwarmPeer data, filtered by recipient capability
        let full_nodes: Vec<(OverlayAddress, SwarmPeer)> = self
            .peer_manager
            .manager
            .connected_peers()
            .into_iter()
            .filter(|overlay| self.peer_manager.is_full_node(overlay))
            .filter_map(|overlay| {
                self.peer_manager
                    .get_peer_snapshot(&overlay)
                    .and_then(|s| s.ext.peer.map(|peer| (overlay, peer)))
            })
            .filter(|(overlay, _)| {
                // Filter by recipient's IP capability using stored peer capability
                let peer_capability = self.get_peer_capability(overlay);
                Self::capabilities_compatible(recipient_capability, peer_capability)
            })
            .collect();

        if full_nodes.is_empty() {
            return selected;
        }

        // Priority 1: Peers close to recipient's overlay
        // Sort by proximity to recipient (descending)
        let mut by_proximity: Vec<_> = full_nodes
            .iter()
            .map(|(overlay, peer)| {
                let prox = recipient.proximity(overlay);
                (prox, overlay, peer)
            })
            .collect();
        by_proximity.sort_by(|a, b| b.0.cmp(&a.0));

        // Take the closest peers
        for (_, _, peer) in by_proximity.iter().take(self.config.close_peers_count) {
            selected.push((*peer).clone());
        }

        // Priority 2: Diverse sample from remaining peers
        // Add peers from different bins for routing diversity
        let mut added_bins = std::collections::HashSet::new();

        for (overlay, peer) in &full_nodes {
            if selected.len() >= self.config.max_peers_for_distant {
                break;
            }

            // Skip if already added
            if selected.iter().any(|p| p.overlay() == peer.overlay()) {
                continue;
            }

            let bin = self.local_overlay.proximity(overlay);
            if !added_bins.contains(&bin) {
                selected.push(peer.clone());
                added_bins.insert(bin);
            }
        }

        // Fill remaining slots with any peers not yet included
        for (_, peer) in &full_nodes {
            if selected.len() >= self.config.max_peers_for_distant {
                break;
            }
            if !selected.iter().any(|p| p.overlay() == peer.overlay()) {
                selected.push(peer.clone());
            }
        }

        selected
    }

    /// Clean up tracking for disconnected peers.
    pub(crate) fn on_peer_disconnected(&mut self, overlay: &OverlayAddress) {
        self.last_broadcast.remove(overlay);
    }

    /// Get the IP capability of a peer (defaults to Both if unknown).
    fn get_peer_capability(&self, overlay: &OverlayAddress) -> IpCapability {
        self.peer_manager
            .get_peer_capability(overlay)
            .unwrap_or(IpCapability::Both) // Conservative: assume dual-stack if unknown
    }

    /// Filter peers to only those reachable by the recipient's IP capability.
    fn filter_peers_for_recipient(
        &self,
        peers: Vec<SwarmPeer>,
        recipient_capability: IpCapability,
    ) -> Vec<SwarmPeer> {
        if recipient_capability == IpCapability::Both {
            // Dual-stack recipient can reach everyone
            return peers;
        }

        peers
            .into_iter()
            .filter(|peer| {
                // Look up the peer's stored capability
                let peer_overlay = OverlayAddress::from(*peer.overlay());
                let peer_capability = self.get_peer_capability(&peer_overlay);

                Self::capabilities_compatible(recipient_capability, peer_capability)
            })
            .collect()
    }

    /// Check if a recipient with the given capability can reach a peer with the given capability.
    fn capabilities_compatible(recipient: IpCapability, peer: IpCapability) -> bool {
        match (recipient, peer) {
            // Recipient can't reach anyone
            (IpCapability::None, _) => false,
            // Dual-stack recipient can reach everyone
            (IpCapability::Both, _) => true,
            // V4-only recipient can reach V4-only or dual-stack peers
            (IpCapability::V4Only, IpCapability::V4Only | IpCapability::Both) => true,
            (IpCapability::V4Only, _) => false,
            // V6-only recipient can reach V6-only or dual-stack peers
            (IpCapability::V6Only, IpCapability::V6Only | IpCapability::Both) => true,
            (IpCapability::V6Only, _) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    // TODO: Add tests with mock peer manager
}
