//! Peer manager wrapping NetPeerManager with Swarm-specific extensions.

use std::sync::Arc;

use libp2p::{Multiaddr, PeerId};
use tracing::{debug, trace, warn};
use vertex_net_local::IpCapability;
use vertex_net_peers::{
    ConnectionState, DEFAULT_BAN_THRESHOLD, DEFAULT_MAX_TRACKED_PEERS, NetPeerManager,
    NetPeerStore, PeerScoreSnapshot, PeerState, PeerStoreError,
};
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_primitives::OverlayAddress;

use crate::PeerSnapshot;
use crate::ext::{SwarmExt, SwarmExtSnapshot};

/// Type alias for Swarm-specific NetPeerManager.
pub type SwarmNetPeerManager = NetPeerManager<OverlayAddress, SwarmExt>;

/// Result of peer registration after handshake completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerReadyResult {
    /// Peer accepted as new connection.
    Accepted,
    /// Peer accepted, replacing an old connection.
    Replaced { old_peer_id: PeerId },
    /// Same peer reconnected (duplicate connection).
    DuplicateConnection,
}

/// Peer lifecycle manager wrapping NetPeerManager with Swarm-specific extensions.
///
/// Peers are only added when SwarmPeer is known (from handshake or Hive gossip).
/// Dial tracking is handled separately by DialTracker.
pub struct PeerManager {
    manager: SwarmNetPeerManager,
    store: Option<Arc<dyn NetPeerStore<OverlayAddress, SwarmExtSnapshot, ()>>>,
}

impl PeerManager {
    /// Create a new peer manager with default settings.
    pub fn new() -> Self {
        Self::with_limits(DEFAULT_BAN_THRESHOLD, Some(DEFAULT_MAX_TRACKED_PEERS))
    }

    /// Create with specified limits.
    pub fn with_limits(ban_threshold: f64, max_tracked_peers: Option<usize>) -> Self {
        Self {
            manager: NetPeerManager::new(ban_threshold, max_tracked_peers),
            store: None,
        }
    }

    /// Create with a peer store for persistence.
    pub fn with_store(
        store: Arc<dyn NetPeerStore<OverlayAddress, SwarmExtSnapshot, ()>>,
    ) -> Result<Self, PeerStoreError> {
        Self::with_store_and_limits(store, DEFAULT_BAN_THRESHOLD, Some(DEFAULT_MAX_TRACKED_PEERS))
    }

    /// Create with store and specified limits.
    pub fn with_store_and_limits(
        store: Arc<dyn NetPeerStore<OverlayAddress, SwarmExtSnapshot, ()>>,
        ban_threshold: f64,
        max_tracked_peers: Option<usize>,
    ) -> Result<Self, PeerStoreError> {
        let mut pm = Self::with_limits(ban_threshold, max_tracked_peers);
        pm.store = Some(store);
        pm.load_from_store()?;
        Ok(pm)
    }

    fn load_from_store(&self) -> Result<(), PeerStoreError> {
        let Some(store) = &self.store else {
            return Ok(());
        };

        let count = self.manager.load_from_store(&**store)?;

        if count > 0 {
            tracing::info!(count, "loaded peers from store");
        }
        Ok(())
    }

    /// Check if a peer is a full node.
    pub fn is_full_node(&self, overlay: &OverlayAddress) -> bool {
        self.manager
            .get_peer(overlay)
            .map(|p| p.ext().full_node)
            .unwrap_or(false)
    }

    /// Get multiaddrs for a peer. Returns None if peer not found.
    pub fn get_multiaddrs(&self, overlay: &OverlayAddress) -> Option<Vec<Multiaddr>> {
        self.manager
            .get_peer(overlay)
            .map(|peer| peer.ext().peer.multiaddrs().to_vec())
    }

    /// Get SwarmPeer for a peer.
    pub fn get_swarm_peer(&self, overlay: &OverlayAddress) -> Option<SwarmPeer> {
        self.manager
            .get_peer(overlay)
            .map(|p| p.ext().peer.clone())
    }

    /// Get disconnected peers that can be dialed.
    pub fn disconnected_peers(&self) -> Vec<OverlayAddress> {
        self.manager.disconnected_peers()
    }

    /// Get SwarmPeers for disconnected peers (for dialing).
    pub fn get_dialable_peers(&self, candidates: &[OverlayAddress]) -> Vec<SwarmPeer> {
        candidates
            .iter()
            .filter_map(|overlay| {
                let peer = self.manager.get_peer(overlay)?;
                if peer.is_disconnected() {
                    Some(peer.ext().peer.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Get a peer's IP capability.
    pub fn get_peer_capability(&self, overlay: &OverlayAddress) -> Option<IpCapability> {
        self.manager
            .get_peer(overlay)
            .map(|ps| ps.ext().ip_capability)
    }

    /// Store a peer discovered via Hive gossip (starts in Disconnected state).
    pub fn store_discovered_peer(&self, swarm_peer: SwarmPeer) -> OverlayAddress {
        let overlay = OverlayAddress::from(*swarm_peer.overlay());
        let ext = SwarmExt::new(swarm_peer, false);
        let peer_state = self.manager.insert_peer(overlay, ext);
        self.persist_peer(&overlay, &peer_state);
        overlay
    }

    /// Store multiple peers discovered via Hive gossip.
    pub fn store_discovered_peers(
        &self,
        peers: impl IntoIterator<Item = SwarmPeer>,
    ) -> Vec<OverlayAddress> {
        let peers: Vec<SwarmPeer> = peers.into_iter().collect();

        if peers.is_empty() {
            return Vec::new();
        }

        debug!(count = peers.len(), "storing discovered peers");

        let mut to_persist = Vec::new();
        let mut stored_overlays = Vec::with_capacity(peers.len());

        for swarm_peer in peers {
            let overlay = OverlayAddress::from(*swarm_peer.overlay());
            let ext = SwarmExt::new(swarm_peer, false);
            let peer_state = self.manager.insert_peer(overlay, ext);

            to_persist.push((overlay, peer_state));
            stored_overlays.push(overlay);
        }

        if let Some(store) = &self.store {
            let snapshots: Vec<PeerSnapshot> = to_persist
                .iter()
                .map(|(overlay, ps)| self.peer_state_to_snapshot(overlay, ps))
                .collect();
            if let Err(e) = store.save_batch(&snapshots) {
                warn!(error = %e, "failed to persist peers batch");
            }
        }

        stored_overlays
    }

    /// Get a peer snapshot by overlay address.
    pub fn get_peer_snapshot(&self, overlay: &OverlayAddress) -> Option<PeerSnapshot> {
        self.manager
            .get_peer(overlay)
            .map(|ps| self.peer_state_to_snapshot(overlay, &ps))
    }

    /// Get all peer snapshots.
    pub fn all_peer_snapshots(&self) -> Vec<PeerSnapshot> {
        self.manager
            .peer_ids()
            .iter()
            .filter_map(|overlay| {
                self.manager
                    .get_peer(overlay)
                    .map(|ps| self.peer_state_to_snapshot(overlay, &ps))
            })
            .collect()
    }

    /// Get peer snapshots for Hive broadcast (non-banned).
    pub fn peers_for_hive_broadcast(&self) -> Vec<PeerSnapshot> {
        self.all_peer_snapshots()
            .into_iter()
            .filter(|p| p.ban_info.is_none())
            .collect()
    }

    /// Ban a peer.
    pub fn ban(&self, overlay: &OverlayAddress, reason: Option<String>) {
        warn!(?overlay, ?reason, "banning peer");
        self.manager.ban(overlay, reason);
        if let Some(peer) = self.manager.get_peer(overlay) {
            self.persist_peer(overlay, &peer);
        }
    }

    /// Get the current score for a peer.
    pub fn peer_score(&self, overlay: &OverlayAddress) -> f64 {
        self.manager.score(overlay).unwrap_or(0.0)
    }

    /// Adjust a peer's score.
    pub fn adjust_score(&self, overlay: &OverlayAddress, delta: f64) {
        if let Some(peer) = self.manager.get_peer(overlay) {
            peer.add_score(delta);
        }
    }

    /// Check if a peer should be banned based on score.
    pub fn should_ban_by_score(&self, overlay: &OverlayAddress) -> bool {
        self.manager
            .get_peer(overlay)
            .map(|p| p.should_ban(self.manager.ban_threshold()))
            .unwrap_or(false)
    }

    /// Check if a peer is banned.
    pub fn is_banned(&self, overlay: &OverlayAddress) -> bool {
        self.manager.is_banned(overlay)
    }

    /// Check if a peer exists.
    pub fn contains(&self, overlay: &OverlayAddress) -> bool {
        self.manager.contains(overlay)
    }

    /// Get all currently connected peers.
    pub fn connected_peers(&self) -> Vec<OverlayAddress> {
        self.manager.connected_peers()
    }

    pub fn flush(&self) -> Result<(), PeerStoreError> {
        if let Some(store) = &self.store {
            store.flush()?;
        }
        Ok(())
    }

    pub fn save_all_to_store(&self) -> Result<usize, PeerStoreError> {
        let Some(store) = &self.store else {
            return Ok(0);
        };
        self.manager.save_to_store(&**store)
    }

    fn persist_peer(&self, overlay: &OverlayAddress, peer_state: &PeerState<OverlayAddress, SwarmExt>) {
        let Some(store) = &self.store else { return };
        let snapshot = self.peer_state_to_snapshot(overlay, peer_state);
        if let Err(e) = store.save(&snapshot) {
            warn!(?overlay, error = %e, "failed to persist peer");
        }
    }

    fn peer_state_to_snapshot(
        &self,
        overlay: &OverlayAddress,
        peer_state: &PeerState<OverlayAddress, SwarmExt>,
    ) -> PeerSnapshot {
        let ext = peer_state.ext();

        let ext_snapshot = SwarmExtSnapshot {
            peer: ext.peer.clone(),
            ip_capability: ext.ip_capability,
            full_node: ext.full_node,
        };

        let multiaddrs = ext.peer.multiaddrs().to_vec();

        PeerSnapshot {
            id: *overlay,
            scoring: PeerScoreSnapshot {
                score: peer_state.score(),
                connection_successes: peer_state.connection_successes(),
                connection_timeouts: peer_state.connection_timeouts(),
                protocol_errors: peer_state.protocol_errors(),
                ..Default::default()
            },
            state: peer_state.connection_state(),
            first_seen: peer_state.first_seen(),
            last_seen: peer_state.last_seen(),
            multiaddrs,
            ban_info: peer_state.ban_info(),
            ext: ext_snapshot,
        }
    }

    /// Get statistics about the peer manager.
    pub fn stats(&self) -> PeerManagerStats {
        let peer_ids = self.manager.peer_ids();
        let total = peer_ids.len();

        let mut connected = 0;
        let mut disconnected = 0;
        let mut banned = 0;
        let mut total_score = 0.0;

        for overlay in &peer_ids {
            if let Some(peer) = self.manager.get_peer(overlay) {
                match peer.connection_state() {
                    ConnectionState::Connected => connected += 1,
                    ConnectionState::Disconnected => disconnected += 1,
                    ConnectionState::Banned => banned += 1,
                }
                total_score += peer.score();
            }
        }

        let avg_score = if total > 0 {
            total_score / total as f64
        } else {
            0.0
        };

        PeerManagerStats {
            total_peers: total,
            connected_peers: connected,
            disconnected_peers: disconnected,
            banned_peers: banned,
            avg_peer_score: avg_score,
        }
    }
}

impl Default for PeerManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PeerManager {
    fn drop(&mut self) {
        if self.store.is_some() {
            match self.save_all_to_store() {
                Ok(count) if count > 0 => {
                    tracing::info!(count, "saved peers on shutdown");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to save peers on shutdown");
                }
                _ => {}
            }
        }
    }
}

/// Statistics about the peer manager state.
#[derive(Debug, Clone)]
pub struct PeerManagerStats {
    pub total_peers: usize,
    pub connected_peers: usize,
    pub disconnected_peers: usize,
    pub banned_peers: usize,
    pub avg_peer_score: f64,
}

/// Bridge trait for operations that require PeerId.
pub trait InternalPeerManager: Send + Sync {
    /// Called when a peer completes handshake. Stores the SwarmPeer.
    fn on_peer_ready(
        &self,
        peer_id: PeerId,
        swarm_peer: SwarmPeer,
        is_full_node: bool,
    ) -> PeerReadyResult;

    /// Called when a peer disconnects.
    fn on_peer_disconnected(&self, peer_id: &PeerId) -> Option<OverlayAddress>;

    /// Resolve an OverlayAddress to its PeerId.
    fn resolve_peer_id(&self, overlay: &OverlayAddress) -> Option<PeerId>;

    /// Resolve a PeerId to its OverlayAddress.
    fn resolve_overlay(&self, peer_id: &PeerId) -> Option<OverlayAddress>;

    /// Record latency for a peer.
    fn record_latency(&self, overlay: &OverlayAddress, rtt: std::time::Duration);
}

impl InternalPeerManager for PeerManager {
    fn on_peer_ready(
        &self,
        peer_id: PeerId,
        swarm_peer: SwarmPeer,
        is_full_node: bool,
    ) -> PeerReadyResult {
        let overlay = OverlayAddress::from(*swarm_peer.overlay());
        debug!(?overlay, %peer_id, is_full_node, "peer ready");

        // Check for existing connection
        let existing_peer_id = self.manager.resolve_peer_id(&overlay);
        let result = if let Some(old_peer_id) = existing_peer_id {
            if old_peer_id == peer_id {
                PeerReadyResult::DuplicateConnection
            } else {
                PeerReadyResult::Replaced { old_peer_id }
            }
        } else {
            PeerReadyResult::Accepted
        };

        // Insert or update peer with SwarmPeer
        let ext = SwarmExt::new(swarm_peer, is_full_node);
        let peer_state = self.manager.insert_peer(overlay, ext);

        // Mark connected
        self.manager.on_connected(&overlay, peer_id);

        // Record success on new connections
        if result == PeerReadyResult::Accepted {
            peer_state.record_success(std::time::Duration::ZERO);
        }

        self.persist_peer(&overlay, &peer_state);

        result
    }

    fn on_peer_disconnected(&self, peer_id: &PeerId) -> Option<OverlayAddress> {
        let overlay = self.manager.on_disconnected_by_peer_id(peer_id)?;
        debug!(?overlay, %peer_id, "peer disconnected");
        Some(overlay)
    }

    fn resolve_peer_id(&self, overlay: &OverlayAddress) -> Option<PeerId> {
        self.manager.resolve_peer_id(overlay)
    }

    fn resolve_overlay(&self, peer_id: &PeerId) -> Option<OverlayAddress> {
        self.manager.resolve_id(peer_id)
    }

    fn record_latency(&self, overlay: &OverlayAddress, rtt: std::time::Duration) {
        if let Some(peer) = self.manager.get_peer(overlay) {
            peer.set_latency(rtt);
            trace!(?overlay, ?rtt, "recorded latency");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256, Signature};

    fn test_overlay(n: u8) -> OverlayAddress {
        OverlayAddress::from(B256::repeat_byte(n))
    }

    fn test_peer_id(n: u8) -> PeerId {
        let bytes = [n; 32];
        let key = libp2p::identity::ed25519::SecretKey::try_from_bytes(bytes).unwrap();
        let keypair =
            libp2p::identity::Keypair::from(libp2p::identity::ed25519::Keypair::from(key));
        keypair.public().to_peer_id()
    }

    fn test_swarm_peer(n: u8) -> SwarmPeer {
        let overlay = B256::repeat_byte(n);
        let multiaddrs = vec![format!("/ip4/127.0.0.{}/tcp/1634", n).parse().unwrap()];
        SwarmPeer::from_validated(
            multiaddrs,
            Signature::test_signature(),
            overlay,
            B256::ZERO,
            Address::ZERO,
        )
    }

    fn get_state(pm: &PeerManager, overlay: &OverlayAddress) -> Option<ConnectionState> {
        pm.manager.get_peer(overlay).map(|p| p.connection_state())
    }

    #[test]
    fn test_store_discovered_peer() {
        let pm = PeerManager::new();
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        let stored = pm.store_discovered_peer(swarm_peer.clone());
        assert_eq!(stored, overlay);
        assert_eq!(get_state(&pm, &overlay), Some(ConnectionState::Disconnected));
        assert!(pm.get_multiaddrs(&overlay).is_some());
    }

    #[test]
    fn test_on_peer_ready() {
        let pm = PeerManager::new();
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);
        let peer_id = test_peer_id(1);

        let result = pm.on_peer_ready(peer_id, swarm_peer, true);
        assert_eq!(result, PeerReadyResult::Accepted);
        assert_eq!(get_state(&pm, &overlay), Some(ConnectionState::Connected));
        assert!(pm.is_full_node(&overlay));
        assert_eq!(pm.resolve_peer_id(&overlay), Some(peer_id));
    }

    #[test]
    fn test_peer_lifecycle() {
        let pm = PeerManager::new();
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);
        let peer_id = test_peer_id(1);

        // Discover peer via Hive
        pm.store_discovered_peer(swarm_peer.clone());
        assert_eq!(get_state(&pm, &overlay), Some(ConnectionState::Disconnected));

        // Should be in dialable list
        assert!(pm.disconnected_peers().contains(&overlay));

        // Connect
        let result = pm.on_peer_ready(peer_id, swarm_peer, false);
        assert_eq!(result, PeerReadyResult::Accepted);
        assert_eq!(get_state(&pm, &overlay), Some(ConnectionState::Connected));

        // Disconnect
        let disconnected = pm.on_peer_disconnected(&peer_id);
        assert_eq!(disconnected, Some(overlay));
        assert_eq!(get_state(&pm, &overlay), Some(ConnectionState::Disconnected));
    }

    #[test]
    fn test_duplicate_connection() {
        let pm = PeerManager::new();
        let swarm_peer = test_swarm_peer(1);
        let peer_id = test_peer_id(1);

        // First connection
        let result1 = pm.on_peer_ready(peer_id, swarm_peer.clone(), false);
        assert_eq!(result1, PeerReadyResult::Accepted);

        // Duplicate from same peer_id
        let result2 = pm.on_peer_ready(peer_id, swarm_peer, false);
        assert_eq!(result2, PeerReadyResult::DuplicateConnection);
    }

    #[test]
    fn test_replaced_connection() {
        let pm = PeerManager::new();
        let swarm_peer = test_swarm_peer(1);
        let peer_id1 = test_peer_id(1);
        let peer_id2 = test_peer_id(2);

        // First connection
        let result1 = pm.on_peer_ready(peer_id1, swarm_peer.clone(), false);
        assert_eq!(result1, PeerReadyResult::Accepted);

        // New connection from different peer_id (same overlay)
        let result2 = pm.on_peer_ready(peer_id2, swarm_peer, false);
        assert_eq!(result2, PeerReadyResult::Replaced { old_peer_id: peer_id1 });
    }

    #[test]
    fn test_ban() {
        let pm = PeerManager::new();
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);
        let peer_id = test_peer_id(1);

        pm.on_peer_ready(peer_id, swarm_peer, false);
        pm.ban(&overlay, Some("misbehaving".to_string()));

        assert_eq!(get_state(&pm, &overlay), Some(ConnectionState::Banned));
        assert!(pm.is_banned(&overlay));
    }

    #[test]
    fn test_get_dialable_peers() {
        let pm = PeerManager::new();

        // Store some peers
        for n in 1..=5 {
            pm.store_discovered_peer(test_swarm_peer(n));
        }

        // Connect one
        pm.on_peer_ready(test_peer_id(1), test_swarm_peer(1), false);

        // Get dialable (disconnected) peers
        let all_overlays: Vec<_> = (1..=5).map(test_overlay).collect();
        let dialable = pm.get_dialable_peers(&all_overlays);

        // Should exclude the connected one
        assert_eq!(dialable.len(), 4);
    }
}
