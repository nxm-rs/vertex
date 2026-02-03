//! Peer manager implementation wrapping NetPeerManager with Swarm-specific extensions.

use std::collections::HashSet;
use std::sync::Arc;

use alloy_primitives::{Address, B256, Signature};
use libp2p::{Multiaddr, PeerId};
use parking_lot::Mutex;
use tracing::{debug, trace, warn};
use vertex_net_peer::IpCapability;
use vertex_net_peers::{
    ConnectionState, NetPeerManager, NetPeerManagerConfig, NetPeerStore, PeerScoreSnapshot,
    PeerState, PeerStoreError,
};
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_primitives::OverlayAddress;

use crate::PeerSnapshot;
use crate::ext::{SwarmExt, SwarmExtSnapshot};
use crate::ip_tracker::{IpScoreTracker, IpTrackerConfig};

/// Type alias for Swarm-specific NetPeerManager.
pub type SwarmNetPeerManager = NetPeerManager<OverlayAddress, SwarmExt>;

/// Reason for connection failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureReason {
    /// Connection timed out.
    Timeout,
    /// Connection was refused.
    Refused,
    /// Handshake failed (identity mismatch, invalid signature, etc).
    HandshakeFailure,
}

/// Result of peer registration after handshake completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerReadyResult {
    /// Peer accepted as new connection.
    Accepted,
    /// Peer accepted, replacing an old connection that should be closed.
    Replaced {
        /// The old PeerId that was replaced.
        old_peer_id: PeerId,
    },
    /// Same peer reconnected (duplicate connection from same PeerId).
    DuplicateConnection,
}

/// Configuration for the peer manager.
#[derive(Debug, Clone, Default)]
pub struct PeerManagerConfig {
    /// Net peer manager configuration.
    pub net_config: NetPeerManagerConfig,
    /// IP tracker configuration.
    pub ip_config: IpTrackerConfig,
}

/// Peer lifecycle manager wrapping NetPeerManager with Swarm-specific extensions.
///
/// # Architecture
///
/// This struct composes [`NetPeerManager`] (generic peer state) with Swarm-specific
/// functionality:
/// - IP-level abuse tracking via [`IpScoreTracker`]
/// - Dial guard to prevent duplicate connection attempts
/// - Hive protocol peer storage via [`PeerStore`]
///
/// # Usage
///
/// Access generic peer operations directly via the `manager` field:
/// ```ignore
/// // Generic operations via manager field
/// pm.manager.is_connected(&overlay)
/// pm.manager.connected_peers()
/// pm.manager.peer(&overlay)
/// ```
///
/// Swarm-specific operations are methods on this struct:
/// ```ignore
/// // Swarm-specific operations
/// pm.store_hive_peer(overlay, addrs, sig, nonce, eth_addr)
/// pm.start_connecting(overlay)  // includes dial-guard
/// pm.ban(overlay, reason)       // includes IP tracking
/// ```
pub struct PeerManager {
    /// Generic peer state management.
    ///
    /// Access this directly for common operations like `is_connected()`,
    /// `connected_peers()`, `peer()`, `resolve_peer_id()`, etc.
    pub manager: SwarmNetPeerManager,

    /// IP-level tracking for abuse prevention.
    ip_tracker: IpScoreTracker,

    /// Overlay addresses with dial in progress (to prevent duplicate dials).
    pending_dials: Mutex<HashSet<OverlayAddress>>,

    /// Optional persistent storage.
    store: Option<Arc<dyn NetPeerStore<OverlayAddress, SwarmExtSnapshot, ()>>>,
}

impl PeerManager {
    /// Create a new peer manager without persistence.
    pub fn new() -> Self {
        Self::with_config(PeerManagerConfig::default())
    }

    /// Create with custom configuration.
    pub fn with_config(config: PeerManagerConfig) -> Self {
        Self {
            manager: NetPeerManager::new(config.net_config),
            ip_tracker: IpScoreTracker::with_config(config.ip_config),
            pending_dials: Mutex::new(HashSet::new()),
            store: None,
        }
    }

    /// Create with a persistent store.
    pub fn with_store(
        store: Arc<dyn NetPeerStore<OverlayAddress, SwarmExtSnapshot, ()>>,
    ) -> Result<Self, PeerStoreError> {
        Self::with_store_and_config(store, PeerManagerConfig::default())
    }

    /// Create with a persistent store and custom configuration.
    pub fn with_store_and_config(
        store: Arc<dyn NetPeerStore<OverlayAddress, SwarmExtSnapshot, ()>>,
        config: PeerManagerConfig,
    ) -> Result<Self, PeerStoreError> {
        let mut pm = Self::with_config(config);
        pm.store = Some(store);
        pm.load_from_store()?;
        Ok(pm)
    }

    /// Load peers from the persistent store.
    fn load_from_store(&self) -> Result<(), PeerStoreError> {
        let store = match &self.store {
            Some(s) => s,
            None => return Ok(()),
        };

        // Use the generic load_from_store via NetPeerManager
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

    /// Get multiaddr addresses for a peer.
    pub fn get_multiaddrs(&self, overlay: &OverlayAddress) -> Option<Vec<Multiaddr>> {
        self.manager.get_peer(overlay).and_then(|peer| {
            // First try SwarmExt (canonical source with signature)
            if let Some(swarm_peer) = peer.ext().swarm_peer() {
                let addrs = swarm_peer.multiaddrs();
                if !addrs.is_empty() {
                    return Some(addrs.to_vec());
                }
            }
            // Fall back to PeerState multiaddrs
            let addrs = peer.multiaddrs();
            if !addrs.is_empty() { Some(addrs) } else { None }
        })
    }

    /// Get all known peers that are dialable (not banned, have multiaddrs).
    pub fn known_dialable_peers(&self) -> Vec<OverlayAddress> {
        self.manager
            .peer_ids()
            .into_iter()
            .filter(|overlay| {
                if let Some(peer) = self.manager.get_peer(overlay) {
                    let state = peer.connection_state();
                    if !state.is_dialable() {
                        return false;
                    }
                    // Must have multiaddrs (either in PeerState or SwarmExt)
                    if !peer.multiaddrs().is_empty() {
                        return true;
                    }
                    if let Some(swarm_peer) = peer.ext().swarm_peer() {
                        return !swarm_peer.multiaddrs().is_empty();
                    }
                }
                false
            })
            .collect()
    }

    /// Get a peer's IP capability.
    pub fn get_peer_capability(&self, overlay: &OverlayAddress) -> Option<IpCapability> {
        self.manager
            .get_peer(overlay)
            .map(|ps| ps.ext().ip_capability)
    }

    /// Store peer data received from hive protocol.
    ///
    /// Creates or updates the peer with BzzAddress data (overlay, multiaddrs,
    /// signature, nonce). The peer is persisted to disk if a store is configured.
    pub fn store_hive_peer(
        &self,
        overlay: B256,
        multiaddrs: Vec<Multiaddr>,
        signature: Signature,
        nonce: B256,
        ethereum_address: Address,
    ) {
        if multiaddrs.is_empty() {
            return;
        }

        let overlay_addr = OverlayAddress::from(overlay);

        // Get or create peer state
        let peer_state = self.manager.peer(overlay_addr);

        // Create SwarmPeer and store in SwarmExt
        let swarm_peer = SwarmPeer::from_validated(
            multiaddrs.clone(),
            signature,
            overlay,
            nonce,
            ethereum_address,
        );

        {
            let mut ext = peer_state.ext_mut();
            ext.set_peer(swarm_peer.clone());
        }

        // Also update PeerState multiaddrs for consistency
        peer_state.update_multiaddrs(multiaddrs);

        // Persist to store
        self.persist_peer(&overlay_addr, &peer_state);
    }

    /// Store multiple peers received from hive protocol in a single batch.
    ///
    /// Returns the overlays of peers that were actually stored (dialable peers with multiaddrs).
    /// Use this return value to add to Kademlia to ensure consistency.
    pub fn store_hive_peers_batch(
        &self,
        peers: impl IntoIterator<Item = SwarmPeer>,
    ) -> Vec<OverlayAddress> {
        let peers: Vec<SwarmPeer> = peers.into_iter().filter(|p| p.is_dialable()).collect();

        if peers.is_empty() {
            return Vec::new();
        }

        debug!(count = peers.len(), "storing hive peers batch");

        let mut to_persist = Vec::new();
        let mut stored_overlays = Vec::with_capacity(peers.len());

        for swarm_peer in peers {
            let overlay = OverlayAddress::from(B256::from_slice(swarm_peer.overlay().as_ref()));
            let peer_state = self.manager.peer(overlay);

            // Store in SwarmExt
            {
                let mut ext = peer_state.ext_mut();
                ext.set_peer(swarm_peer.clone());
            }

            // Update PeerState multiaddrs
            peer_state.update_multiaddrs(swarm_peer.multiaddrs().to_vec());

            to_persist.push((overlay, peer_state));
            stored_overlays.push(overlay);
        }

        // Batch persist
        if let Some(store) = &self.store {
            let snapshots: Vec<PeerSnapshot> = to_persist
                .iter()
                .filter_map(|(overlay, ps)| self.peer_state_to_snapshot(overlay, ps))
                .collect();
            if let Err(e) = store.save_batch(&snapshots) {
                warn!(error = %e, "failed to persist hive peers batch");
            }
        }

        stored_overlays
    }

    /// Store a peer snapshot directly.
    pub fn store_peer(&self, snapshot: PeerSnapshot) {
        let overlay = snapshot.id;
        let peer_state = self.manager.peer(overlay);

        // Restore SwarmExt from snapshot extension
        if let Some(swarm_peer) = snapshot.ext.peer.as_ref() {
            peer_state.ext_mut().set_peer(swarm_peer.clone());
        }
        peer_state.ext_mut().full_node = snapshot.ext.full_node;
        peer_state.ext_mut().ip_capability = snapshot.ext.ip_capability;

        // Restore other state
        peer_state.update_multiaddrs(snapshot.multiaddrs.clone());
        peer_state.set_score(snapshot.scoring.score);

        if let Some(ban_info) = &snapshot.ban_info {
            peer_state.ban(ban_info.reason.clone());
        }

        // Persist
        self.persist_peer(&overlay, &peer_state);
    }

    /// Get a peer snapshot by overlay address.
    pub fn get_peer_snapshot(&self, overlay: &OverlayAddress) -> Option<PeerSnapshot> {
        self.manager
            .get_peer(overlay)
            .and_then(|ps| self.peer_state_to_snapshot(overlay, &ps))
    }

    /// Get all peer snapshots (for Hive broadcasting).
    pub fn all_peer_snapshots(&self) -> Vec<PeerSnapshot> {
        self.manager
            .peer_ids()
            .iter()
            .filter_map(|overlay| {
                self.manager
                    .get_peer(overlay)
                    .and_then(|ps| self.peer_state_to_snapshot(overlay, &ps))
            })
            .collect()
    }

    /// Get peer snapshots suitable for Hive broadcast (non-banned with valid signatures).
    pub fn peers_for_hive_broadcast(&self) -> Vec<PeerSnapshot> {
        self.all_peer_snapshots()
            .into_iter()
            .filter(|p| p.ban_info.is_none())
            .collect()
    }

    /// Mark a peer as "connecting" to prevent duplicate dials.
    ///
    /// Returns true if the transition was successful (peer was dialable).
    /// Returns false if peer is already connecting, connected, or banned.
    pub fn start_connecting(&self, overlay: OverlayAddress) -> bool {
        // Check pending_dials first (atomic check-and-set)
        {
            let mut pending = self.pending_dials.lock();
            if !pending.insert(overlay) {
                debug!(?overlay, "dial already in progress");
                return false;
            }
        }

        // Try to start connecting via manager
        let success = self.manager.start_connecting(overlay);

        if !success {
            // Rollback pending_dials
            self.pending_dials.lock().remove(&overlay);
            debug!(?overlay, "peer not dialable");
            return false;
        }

        debug!(?overlay, "starting connection");
        true
    }

    /// Mark a connection attempt as failed with a timeout.
    pub fn connection_timeout(&self, overlay: &OverlayAddress) {
        self.connection_failed_internal(overlay, FailureReason::Timeout);
    }

    /// Mark a connection attempt as refused.
    pub fn connection_refused(&self, overlay: &OverlayAddress) {
        self.connection_failed_internal(overlay, FailureReason::Refused);
    }

    /// Mark a connection attempt as failed due to handshake error.
    pub fn handshake_failed(&self, overlay: &OverlayAddress) {
        self.connection_failed_internal(overlay, FailureReason::HandshakeFailure);
    }

    /// Mark a connection attempt as failed (generic).
    pub fn connection_failed(&self, overlay: &OverlayAddress) {
        self.connection_failed_internal(overlay, FailureReason::Timeout);
    }

    fn connection_failed_internal(&self, overlay: &OverlayAddress, reason: FailureReason) {
        self.pending_dials.lock().remove(overlay);

        // Update score based on failure reason
        if let Some(peer) = self.manager.get_peer(overlay) {
            match reason {
                FailureReason::Timeout => peer.record_timeout(),
                FailureReason::Refused => peer.add_score(-1.0),
                FailureReason::HandshakeFailure => peer.add_score(-5.0),
            }
        }

        // Transition to disconnected
        self.manager.on_disconnected(overlay);

        debug!(?overlay, ?reason, "connection failed");
    }

    /// Check if a dial is pending for this overlay.
    pub fn is_dial_pending(&self, overlay: &OverlayAddress) -> bool {
        self.pending_dials.lock().contains(overlay)
    }

    /// Filter candidates to find peers that are dialable and have stored multiaddrs.
    pub fn filter_dialable_candidates(
        &self,
        candidates: &[OverlayAddress],
    ) -> Vec<(OverlayAddress, Vec<Multiaddr>)> {
        let pending = self.pending_dials.lock();

        candidates
            .iter()
            .filter(|overlay| {
                // Skip if dial pending
                if pending.contains(overlay) {
                    return false;
                }
                // Check if dialable
                if let Some(peer) = self.manager.get_peer(overlay) {
                    return peer.connection_state().is_dialable();
                }
                true // Unknown peers are dialable
            })
            .filter_map(|overlay| self.get_multiaddrs(overlay).map(|addrs| (*overlay, addrs)))
            .collect()
    }

    /// Add a known peer without connecting.
    pub fn add_known(&self, overlay: OverlayAddress) {
        // Just access the peer to create it if it doesn't exist
        let _ = self.manager.peer(overlay);
    }

    /// Ban a peer. They will not be reconnected.
    ///
    /// Also records the ban in the IP tracker for abuse prevention.
    pub fn ban(&self, overlay: OverlayAddress, reason: Option<String>) {
        warn!(?overlay, ?reason, "banning peer");

        // Remove from pending dials
        self.pending_dials.lock().remove(&overlay);

        // Ban via manager
        self.manager.ban(overlay, reason.clone());

        // Record ban in IP tracker
        self.ip_tracker.record_overlay_banned(&overlay);

        // Persist
        if let Some(peer) = self.manager.get_peer(&overlay) {
            self.persist_peer(&overlay, &peer);
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
        if let Some(peer) = self.manager.get_peer(overlay) {
            return peer.should_ban(self.manager.config().ban_threshold);
        }
        false
    }

    /// Rank overlays by score (highest first).
    pub fn rank_by_score(&self, overlays: &[OverlayAddress]) -> Vec<(OverlayAddress, f64)> {
        let mut ranked: Vec<_> = overlays
            .iter()
            .map(|o| {
                let score = self.manager.score(o).unwrap_or(0.0);
                (*o, score)
            })
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranked
    }

    /// Check if an IP is banned.
    pub fn is_ip_banned(&self, ip: &std::net::IpAddr) -> bool {
        self.ip_tracker.is_ip_banned(ip)
    }

    /// Ban an IP address.
    pub fn ban_ip(&self, ip: std::net::IpAddr, reason: Option<String>) {
        self.ip_tracker.ban_ip(ip, reason);
    }

    /// Associate an IP with a peer (for abuse tracking).
    pub fn associate_peer_ip(&self, overlay: OverlayAddress, ip: std::net::IpAddr) {
        self.ip_tracker.associate_ip(overlay, ip);
    }

    /// Get access to the underlying IP tracker.
    pub fn ip_tracker(&self) -> &IpScoreTracker {
        &self.ip_tracker
    }

    /// Flush all pending changes to the persistent store.
    pub fn flush(&self) -> Result<(), PeerStoreError> {
        if let Some(store) = &self.store {
            store.flush()?;
        }
        Ok(())
    }

    /// Save all peers to the persistent store.
    ///
    /// This saves the complete in-memory state of all peers to the store,
    /// ensuring all state changes (scores, connection states, etc.) are persisted.
    /// Called automatically on drop.
    pub fn save_all_to_store(&self) -> Result<usize, PeerStoreError> {
        let store = match &self.store {
            Some(s) => s,
            None => return Ok(0),
        };

        self.manager.save_to_store(&**store)
    }

    fn persist_peer(
        &self,
        overlay: &OverlayAddress,
        peer_state: &PeerState<OverlayAddress, SwarmExt>,
    ) {
        if let Some(store) = &self.store {
            if let Some(snapshot) = self.peer_state_to_snapshot(overlay, peer_state) {
                if let Err(e) = store.save(&snapshot) {
                    warn!(?overlay, error = %e, "failed to persist peer");
                }
            }
        }
    }

    fn peer_state_to_snapshot(
        &self,
        overlay: &OverlayAddress,
        peer_state: &PeerState<OverlayAddress, SwarmExt>,
    ) -> Option<PeerSnapshot> {
        let ext = peer_state.ext();

        // Build SwarmExtSnapshot
        let ext_snapshot = SwarmExtSnapshot {
            peer: ext.swarm_peer().cloned(),
            ip_capability: ext.ip_capability,
            full_node: ext.full_node,
        };

        // Get multiaddrs - prefer SwarmPeer multiaddrs, fall back to PeerState
        let multiaddrs = ext
            .swarm_peer()
            .map(|p| p.multiaddrs().to_vec())
            .unwrap_or_else(|| peer_state.multiaddrs());

        Some(PeerSnapshot {
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
        })
    }

    /// Get statistics about the peer manager.
    pub fn stats(&self) -> PeerManagerStats {
        let ip_stats = self.ip_tracker.stats();
        let peer_ids = self.manager.peer_ids();
        let total = peer_ids.len();

        let mut connected = 0;
        let mut known = 0;
        let mut banned = 0;
        let mut stored = 0;
        let mut total_score = 0.0;

        for overlay in &peer_ids {
            if let Some(peer) = self.manager.get_peer(overlay) {
                match peer.connection_state() {
                    ConnectionState::Connected => connected += 1,
                    ConnectionState::Known => known += 1,
                    ConnectionState::Banned => banned += 1,
                    _ => {}
                }
                if peer.ext().has_identity() {
                    stored += 1;
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
            known_peers: known,
            banned_peers: banned,
            stored_peers: stored,
            avg_peer_score: avg_score,
            banned_ips: ip_stats.banned_ips,
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
                Ok(count) => {
                    if count > 0 {
                        tracing::info!(count, "saved peers to store on shutdown");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to save peers on shutdown");
                }
            }
        }
    }
}

/// Statistics about the peer manager state.
#[derive(Debug, Clone)]
pub struct PeerManagerStats {
    /// Total number of peers tracked.
    pub total_peers: usize,
    /// Number of currently connected peers.
    pub connected_peers: usize,
    /// Number of known (but not connected) peers.
    pub known_peers: usize,
    /// Number of banned peers.
    pub banned_peers: usize,
    /// Number of stored peers (with full SwarmPeer identity).
    pub stored_peers: usize,
    /// Average peer score.
    pub avg_peer_score: f64,
    /// Number of banned IPs.
    pub banned_ips: usize,
}

/// Bridge trait for operations that require PeerId.
///
/// Implemented by PeerManager for the boundary between libp2p and Swarm layers.
/// Only the bridge layer (topology behaviour, swarm node) should use this trait.
pub trait InternalPeerManager: Send + Sync {
    /// Called when a peer completes handshake.
    fn on_peer_ready(
        &self,
        peer_id: PeerId,
        overlay: OverlayAddress,
        is_full_node: bool,
    ) -> PeerReadyResult;

    /// Called when a peer disconnects. Returns the OverlayAddress if known.
    fn on_peer_disconnected(&self, peer_id: &PeerId) -> Option<OverlayAddress>;

    /// Resolve an OverlayAddress to its PeerId.
    fn resolve_peer_id(&self, overlay: &OverlayAddress) -> Option<PeerId>;

    /// Resolve a PeerId to its OverlayAddress.
    fn resolve_overlay(&self, peer_id: &PeerId) -> Option<OverlayAddress>;

    /// Record latency for a peer from a ping/pong exchange.
    fn record_latency(&self, overlay: &OverlayAddress, rtt: std::time::Duration);
}

impl InternalPeerManager for PeerManager {
    fn on_peer_ready(
        &self,
        peer_id: PeerId,
        overlay: OverlayAddress,
        is_full_node: bool,
    ) -> PeerReadyResult {
        debug!(?overlay, %peer_id, is_full_node, "peer ready");

        // Remove from pending dials
        self.pending_dials.lock().remove(&overlay);

        // Check for existing registration
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

        // Register via manager
        self.manager.on_connected(overlay, peer_id);

        // Set full_node flag in SwarmExt
        if let Some(peer) = self.manager.get_peer(&overlay) {
            peer.ext_mut().full_node = is_full_node;
        }

        // Record success for new connections
        if result == PeerReadyResult::Accepted {
            if let Some(peer) = self.manager.get_peer(&overlay) {
                peer.record_success(std::time::Duration::ZERO);
            }
        }

        // Persist
        if let Some(peer) = self.manager.get_peer(&overlay) {
            self.persist_peer(&overlay, &peer);
        }

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
            trace!(?overlay, ?rtt, "recorded peer latency");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;

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

    fn get_state(pm: &PeerManager, overlay: &OverlayAddress) -> Option<ConnectionState> {
        pm.manager.get_peer(overlay).map(|p| p.connection_state())
    }

    #[test]
    fn test_peer_lifecycle() {
        let pm = PeerManager::new();
        let overlay = test_overlay(1);
        let peer_id = test_peer_id(1);

        // Initially unknown
        assert!(get_state(&pm, &overlay).is_none());
        assert!(!pm.manager.is_connected(&overlay));

        // Add as known
        pm.add_known(overlay);
        assert_eq!(get_state(&pm, &overlay), Some(ConnectionState::Known));

        // Start connecting
        assert!(pm.start_connecting(overlay));
        assert_eq!(get_state(&pm, &overlay), Some(ConnectionState::Connecting));
        assert!(pm.is_dial_pending(&overlay));

        // Can't start connecting again
        assert!(!pm.start_connecting(overlay));

        // Peer connects
        pm.on_peer_ready(peer_id, overlay, true);
        assert_eq!(get_state(&pm, &overlay), Some(ConnectionState::Connected));
        assert!(pm.manager.is_connected(&overlay));
        assert!(!pm.is_dial_pending(&overlay));
        assert_eq!(pm.manager.connected_count(), 1);

        // Resolve peer_id
        assert_eq!(pm.resolve_peer_id(&overlay), Some(peer_id));

        // Disconnect
        let disconnected = pm.on_peer_disconnected(&peer_id);
        assert_eq!(disconnected, Some(overlay));
        assert_eq!(
            get_state(&pm, &overlay),
            Some(ConnectionState::Disconnected)
        );
        assert!(!pm.manager.is_connected(&overlay));
        assert_eq!(pm.manager.connected_count(), 0);
    }

    #[test]
    fn test_connection_failure() {
        let pm = PeerManager::new();
        let overlay = test_overlay(1);

        pm.add_known(overlay);
        assert!(pm.start_connecting(overlay));

        pm.connection_failed(&overlay);
        assert_eq!(
            get_state(&pm, &overlay),
            Some(ConnectionState::Disconnected)
        );
        assert!(!pm.is_dial_pending(&overlay));
    }

    #[test]
    fn test_ban() {
        let pm = PeerManager::new();
        let overlay = test_overlay(1);
        let peer_id = test_peer_id(1);

        // Connect then ban
        pm.on_peer_ready(peer_id, overlay, false);
        pm.ban(overlay, Some("misbehaving".to_string()));

        assert_eq!(get_state(&pm, &overlay), Some(ConnectionState::Banned));
        assert!(!pm.start_connecting(overlay)); // Can't dial banned peer
        assert!(pm.resolve_peer_id(&overlay).is_none()); // Mapping removed
    }

    fn store_test_peer(pm: &PeerManager, overlay: OverlayAddress, addrs: Vec<Multiaddr>) {
        let overlay_b256 = B256::from(*overlay);
        pm.store_hive_peer(
            overlay_b256,
            addrs,
            Signature::test_signature(),
            B256::ZERO,
            Address::ZERO,
        );
    }

    #[test]
    fn test_store_peer_multiaddrs() {
        let pm = PeerManager::new();
        let overlay = test_overlay(1);
        let addrs: Vec<Multiaddr> = vec!["/ip4/127.0.0.1/tcp/1234".parse().unwrap()];

        store_test_peer(&pm, overlay, addrs.clone());

        // Should create Known peer with multiaddrs
        assert_eq!(get_state(&pm, &overlay), Some(ConnectionState::Known));
        assert_eq!(pm.get_multiaddrs(&overlay), Some(addrs));
    }

    #[test]
    fn test_store_peers_batch() {
        let pm = PeerManager::new();

        for n in 1u8..=5 {
            let overlay = test_overlay(n);
            let addrs: Vec<Multiaddr> =
                vec![format!("/ip4/127.0.0.{}/tcp/1234", n).parse().unwrap()];
            store_test_peer(&pm, overlay, addrs);
        }

        // All should be known with stored multiaddrs
        for n in 1u8..=5 {
            let overlay = test_overlay(n);
            let expected: Vec<Multiaddr> =
                vec![format!("/ip4/127.0.0.{}/tcp/1234", n).parse().unwrap()];
            assert_eq!(get_state(&pm, &overlay), Some(ConnectionState::Known));
            assert_eq!(pm.get_multiaddrs(&overlay), Some(expected));
        }
    }

    #[test]
    fn test_filter_dialable_candidates() {
        let pm = PeerManager::new();

        // Setup: 5 peers with different states
        let overlays: Vec<_> = (1..=5).map(test_overlay).collect();
        let peer_ids: Vec<_> = (1..=5).map(test_peer_id).collect();

        // Peer 1: Known with multiaddrs (dialable)
        let addr1: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();
        store_test_peer(&pm, overlays[0], vec![addr1.clone()]);

        // Peer 2: Connected (not dialable)
        store_test_peer(
            &pm,
            overlays[1],
            vec!["/ip4/127.0.0.2/tcp/1234".parse().unwrap()],
        );
        pm.on_peer_ready(peer_ids[1], overlays[1], false);

        // Peer 3: Dial pending (not dialable)
        store_test_peer(
            &pm,
            overlays[2],
            vec!["/ip4/127.0.0.3/tcp/1234".parse().unwrap()],
        );
        pm.start_connecting(overlays[2]);

        // Peer 4: Banned (not dialable)
        store_test_peer(
            &pm,
            overlays[3],
            vec!["/ip4/127.0.0.4/tcp/1234".parse().unwrap()],
        );
        pm.ban(overlays[3], None);

        // Peer 5: Known but no multiaddrs (not returned)
        pm.add_known(overlays[4]);

        // Filter candidates
        let dialable = pm.filter_dialable_candidates(&overlays);

        // Only peer 1 should be dialable
        assert_eq!(dialable.len(), 1);
        assert_eq!(dialable[0].0, overlays[0]);
        assert_eq!(dialable[0].1, vec![addr1]);
    }

    #[test]
    fn test_manager_access() {
        // Test that manager field provides direct access to generic operations
        let pm = PeerManager::new();
        let overlay = test_overlay(1);
        let peer_id = test_peer_id(1);

        // Access via manager field
        assert!(!pm.manager.contains(&overlay));

        pm.add_known(overlay);
        assert!(pm.manager.contains(&overlay));

        pm.on_peer_ready(peer_id, overlay, true);
        assert!(pm.manager.is_connected(&overlay));
        assert_eq!(pm.manager.connected_count(), 1);
        assert_eq!(pm.manager.connected_peers(), vec![overlay]);
    }
}
