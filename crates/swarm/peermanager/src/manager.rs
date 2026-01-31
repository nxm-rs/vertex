//! Peer manager implementation.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use alloy_primitives::{Address, B256, Signature};
use libp2p::{Multiaddr, PeerId};
use parking_lot::{Mutex, RwLock};
use tracing::{debug, info, trace, warn};
use vertex_swarm_peer::SwarmPeer;
use vertex_primitives::OverlayAddress;

use crate::multiaddr_cache::{MultiAddrCache, MultiAddrRegistry};
use crate::score::{ScoreConfig, ScoreHandle, ScoreManager};
use crate::state::{PeerInfo, PeerState, StoredPeer};
use crate::store::{PeerStore, PeerStoreError};

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

/// Peer lifecycle manager.
///
/// Manages peer state and provides a clean abstraction boundary:
/// - Public API uses only OverlayAddress (Swarm layer)
/// - Internal bridge API (via InternalPeerManager trait) handles PeerId mapping
///
/// # Persistence
///
/// When created with a [`PeerStore`], the manager will:
/// - Load known peers from storage on startup
/// - Persist peer updates (connections, bans) to storage
/// - Provide `flush()` to ensure all changes are written
///
/// # Lock Strategy
///
/// - `peers: RwLock` - Read-heavy (state queries)
/// - `registry: Mutex` - Low contention (connect/disconnect only)
/// - `multiaddr_cache: RwLock` - Frequent reads for dial lookups
/// - `pending_dials: Mutex` - Low contention
/// - `store: Option<Arc>` - No lock needed (store handles its own synchronization)
pub struct PeerManager {
    /// Peer state indexed by overlay address.
    peers: RwLock<HashMap<OverlayAddress, PeerInfo>>,
    /// Internal overlay ↔ peer ID mapping.
    registry: Mutex<MultiAddrRegistry>,
    /// Cached multiaddr addresses for dialing.
    multiaddr_cache: RwLock<MultiAddrCache>,
    /// Overlay addresses with dial in progress.
    pending_dials: Mutex<HashSet<OverlayAddress>>,
    /// Optional persistent storage for peers.
    store: Option<Arc<dyn PeerStore>>,
    /// Stored peer data (BzzAddress + stats) for persistence.
    /// This is separate from `peers` because StoredPeer has different data.
    stored_peers: RwLock<HashMap<B256, StoredPeer>>,
    /// Peer and IP scoring manager.
    scores: ScoreManager,
}

impl PeerManager {
    /// Create a new peer manager without persistence.
    pub fn new() -> Self {
        Self::with_score_config(ScoreConfig::default())
    }

    /// Create a new peer manager with custom score configuration.
    pub fn with_score_config(score_config: ScoreConfig) -> Self {
        Self {
            peers: RwLock::new(HashMap::new()),
            registry: Mutex::new(MultiAddrRegistry::new()),
            multiaddr_cache: RwLock::new(MultiAddrCache::default()),
            pending_dials: Mutex::new(HashSet::new()),
            store: None,
            stored_peers: RwLock::new(HashMap::new()),
            scores: ScoreManager::with_config(score_config),
        }
    }

    /// Create a new peer manager with a persistent store.
    ///
    /// Peers will be loaded from the store on creation.
    pub fn with_store(store: Arc<dyn PeerStore>) -> Result<Self, PeerStoreError> {
        Self::with_store_and_config(store, ScoreConfig::default())
    }

    /// Create a new peer manager with a persistent store and custom score config.
    pub fn with_store_and_config(
        store: Arc<dyn PeerStore>,
        score_config: ScoreConfig,
    ) -> Result<Self, PeerStoreError> {
        let mut manager = Self {
            peers: RwLock::new(HashMap::new()),
            registry: Mutex::new(MultiAddrRegistry::new()),
            multiaddr_cache: RwLock::new(MultiAddrCache::default()),
            pending_dials: Mutex::new(HashSet::new()),
            store: Some(store),
            stored_peers: RwLock::new(HashMap::new()),
            scores: ScoreManager::with_config(score_config),
        };
        manager.load_from_store()?;
        Ok(manager)
    }

    /// Load peers from the persistent store.
    ///
    /// This populates the multiaddr cache and peer info from stored data,
    /// and restores score snapshots to the ScoreManager.
    fn load_from_store(&mut self) -> Result<(), PeerStoreError> {
        let store = match &self.store {
            Some(s) => s,
            None => return Ok(()),
        };

        let stored = store.load_all()?;
        let count = stored.len();

        if count == 0 {
            return Ok(());
        }

        let mut peers = self.peers.write();
        let mut cache = self.multiaddr_cache.write();
        let mut stored_map = self.stored_peers.write();
        let mut score_snapshots = Vec::with_capacity(count);

        for peer in stored {
            let overlay = OverlayAddress::from(peer.overlay());

            // Skip banned peers from being dialable, but still track them
            let state = if peer.is_banned() {
                PeerState::Banned
            } else {
                PeerState::Known
            };

            // Create runtime PeerInfo
            let mut info = PeerInfo::new_known();
            info.state = state;
            info.is_full_node = peer.is_full_node;
            if let Some(ban_info) = &peer.ban_info {
                info.ban_reason = ban_info.reason.clone();
            }
            peers.insert(overlay, info);

            // Populate multiaddr cache (for dialing)
            let multiaddrs = peer.multiaddrs();
            if !multiaddrs.is_empty() {
                cache.insert(overlay, multiaddrs.to_vec());
            }

            // Collect score snapshot for restoration
            score_snapshots.push((overlay, peer.score.clone()));

            // Keep stored peer data
            stored_map.insert(peer.overlay(), peer);
        }

        // Release locks before restoring scores
        drop(peers);
        drop(cache);
        drop(stored_map);

        // Restore score snapshots to ScoreManager
        self.scores.restore_snapshots(score_snapshots);

        info!(count, "loaded peers from store");
        Ok(())
    }

    // =========================================================================
    // Public API - OverlayAddress only
    // =========================================================================

    /// Get the current state of a peer.
    pub fn state(&self, overlay: &OverlayAddress) -> Option<PeerState> {
        self.peers.read().get(overlay).map(|info| info.state)
    }

    /// Check if a peer is currently connected.
    pub fn is_connected(&self, overlay: &OverlayAddress) -> bool {
        self.peers
            .read()
            .get(overlay)
            .map(|info| info.state.is_connected())
            .unwrap_or(false)
    }

    /// Get all currently connected peers.
    pub fn connected_peers(&self) -> Vec<OverlayAddress> {
        self.peers
            .read()
            .iter()
            .filter(|(_, info)| info.state.is_connected())
            .map(|(overlay, _)| *overlay)
            .collect()
    }

    /// Get all known peers that are dialable (not banned, have multiaddrs).
    ///
    /// This is used to seed Kademlia on startup with previously known peers.
    pub fn known_dialable_peers(&self) -> Vec<OverlayAddress> {
        let peers = self.peers.read();
        let cache = self.multiaddr_cache.read();

        peers
            .iter()
            .filter(|(overlay, info)| {
                // Must not be banned and must have cached multiaddrs
                !info.state.is_banned() && cache.peek(overlay).is_some()
            })
            .map(|(overlay, _)| *overlay)
            .collect()
    }

    /// Get the number of connected peers.
    pub fn connected_count(&self) -> usize {
        self.peers
            .read()
            .values()
            .filter(|info| info.state.is_connected())
            .count()
    }

    /// Get peer info if it exists.
    pub fn get_info(&self, overlay: &OverlayAddress) -> Option<PeerInfo> {
        self.peers.read().get(overlay).cloned()
    }

    /// Get cached multiaddr addresses for a peer.
    pub fn get_multiaddrs(&self, overlay: &OverlayAddress) -> Option<Vec<Multiaddr>> {
        self.multiaddr_cache.read().peek(overlay).cloned()
    }

    /// Cache multiaddr addresses for a peer.
    ///
    /// Called when we receive peer info from the hive protocol.
    /// For batch operations, prefer [`cache_multiaddrs_batch`] to reduce lock contention.
    pub fn cache_multiaddrs(&self, overlay: OverlayAddress, multiaddrs: Vec<Multiaddr>) {
        if multiaddrs.is_empty() {
            return;
        }

        trace!(?overlay, count = multiaddrs.len(), "caching multiaddrs");
        self.multiaddr_cache.write().insert(overlay, multiaddrs);

        // Ensure peer exists in known state if not already tracked
        let mut peers = self.peers.write();
        peers.entry(overlay).or_insert_with(PeerInfo::new_known);
    }

    /// Cache multiaddr addresses for multiple peers in a single operation.
    ///
    /// This is more efficient than calling [`cache_multiaddrs`] in a loop because
    /// it acquires each lock only once for the entire batch.
    pub fn cache_multiaddrs_batch(
        &self,
        entries: impl IntoIterator<Item = (OverlayAddress, Vec<Multiaddr>)>,
    ) {
        let entries: Vec<_> = entries
            .into_iter()
            .filter(|(_, addrs)| !addrs.is_empty())
            .collect();

        if entries.is_empty() {
            return;
        }

        debug!(count = entries.len(), "caching multiaddrs batch");

        // Single lock acquisition for cache
        {
            let mut cache = self.multiaddr_cache.write();
            for (overlay, multiaddrs) in &entries {
                cache.insert(*overlay, multiaddrs.clone());
            }
        }

        // Single lock acquisition for peers
        {
            let mut peers = self.peers.write();
            for (overlay, _) in entries {
                peers.entry(overlay).or_insert_with(PeerInfo::new_known);
            }
        }
    }

    /// Store peer data received from hive protocol.
    ///
    /// This creates or updates the stored peer with BzzAddress data (overlay, multiaddrs,
    /// signature, nonce). The peer is persisted to disk if a store is configured.
    ///
    /// Also caches the multiaddrs for dialing.
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

        // Cache multiaddrs for dialing
        self.multiaddr_cache
            .write()
            .insert(overlay_addr, multiaddrs.clone());

        // Ensure peer exists in runtime state
        self.peers
            .write()
            .entry(overlay_addr)
            .or_insert_with(PeerInfo::new_known);

        // Create or update stored peer
        {
            let mut stored = self.stored_peers.write();
            if stored.contains_key(&overlay) {
                // Already have this peer, just update multiaddrs if changed
                if let Some(peer) = stored.get_mut(&overlay) {
                    peer.update_multiaddrs(multiaddrs);
                }
            } else {
                // New peer - create stored record
                let peer = StoredPeer::from_components(overlay, multiaddrs, signature, nonce, ethereum_address, false);
                stored.insert(overlay, peer);
            }
        }

        // Persist to store
        if let Some(store) = &self.store {
            if let Some(peer) = self.stored_peers.read().get(&overlay) {
                if let Err(e) = store.save(peer) {
                    warn!(?overlay_addr, error = %e, "failed to persist hive peer");
                }
            }
        }
    }

    /// Store multiple peers received from hive protocol in a single batch.
    ///
    /// More efficient than calling [`store_hive_peer`] in a loop.
    pub fn store_hive_peers_batch(&self, peers: impl IntoIterator<Item = SwarmPeer>) {
        let peers: Vec<SwarmPeer> = peers
            .into_iter()
            .filter(|p| p.is_dialable())
            .collect();

        if peers.is_empty() {
            return;
        }

        debug!(count = peers.len(), "storing hive peers batch");

        // Cache multiaddrs
        {
            let mut cache = self.multiaddr_cache.write();
            for peer in &peers {
                let overlay = OverlayAddress::from(B256::from_slice(peer.overlay().as_ref()));
                cache.insert(overlay, peer.multiaddrs().to_vec());
            }
        }

        // Update runtime peers
        {
            let mut runtime = self.peers.write();
            for peer in &peers {
                let overlay = OverlayAddress::from(B256::from_slice(peer.overlay().as_ref()));
                runtime
                    .entry(overlay)
                    .or_insert_with(PeerInfo::new_known);
            }
        }

        // Create/update stored peers
        let to_persist: Vec<StoredPeer> = {
            let mut stored = self.stored_peers.write();
            let mut to_persist = Vec::new();

            for peer in peers {
                let overlay = B256::from_slice(peer.overlay().as_ref());
                if stored.contains_key(&overlay) {
                    if let Some(stored_peer) = stored.get_mut(&overlay) {
                        stored_peer.update_multiaddrs(peer.multiaddrs().to_vec());
                        to_persist.push(stored_peer.clone());
                    }
                } else {
                    let stored_peer = StoredPeer::new(peer, false);
                    stored.insert(overlay, stored_peer.clone());
                    to_persist.push(stored_peer);
                }
            }

            to_persist
        };

        // Batch persist to store
        if let Some(store) = &self.store {
            if let Err(e) = store.save_batch(&to_persist) {
                warn!(error = %e, "failed to persist hive peers batch");
            }
        }
    }

    /// Mark a peer as "connecting" to prevent duplicate dials.
    ///
    /// Returns true if the transition was successful (peer was dialable).
    /// Returns false if peer is already connecting, connected, or banned.
    pub fn start_connecting(&self, overlay: OverlayAddress) -> bool {
        // Step 1: Try to insert into pending_dials (atomic check-and-set)
        // This prevents duplicate dials without holding the lock long
        {
            let mut pending = self.pending_dials.lock();
            if !pending.insert(overlay) {
                debug!(?overlay, "dial already in progress");
                return false;
            }
        } // pending_dials lock released

        // Step 2: Check peer state and update if dialable
        let dialable = {
            let mut peers = self.peers.write();
            let info = peers.entry(overlay).or_insert_with(PeerInfo::new_known);

            if info.state.is_dialable() {
                info.transition_to(PeerState::Connecting);
                true
            } else {
                debug!(?overlay, state = ?info.state, "peer not dialable");
                false
            }
        }; // peers lock released

        // Step 3: If not dialable, remove from pending (rollback)
        if !dialable {
            self.pending_dials.lock().remove(&overlay);
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

    /// Mark a connection attempt as failed (generic, for backwards compatibility).
    pub fn connection_failed(&self, overlay: &OverlayAddress) {
        self.connection_failed_internal(overlay, FailureReason::Timeout);
    }

    fn connection_failed_internal(&self, overlay: &OverlayAddress, reason: FailureReason) {
        self.pending_dials.lock().remove(overlay);

        // Update score
        let handle = self.scores.handle_for(*overlay);
        match reason {
            FailureReason::Timeout => handle.record_connection_timeout(),
            FailureReason::Refused => handle.record_connection_refused(),
            FailureReason::HandshakeFailure => handle.record_handshake_failure(),
        }

        // Update stored peer score
        let key = B256::from(**overlay);
        if let Some(peer) = self.stored_peers.write().get_mut(&key) {
            match reason {
                FailureReason::Timeout => peer.record_timeout(),
                FailureReason::Refused => peer.record_refused(),
                FailureReason::HandshakeFailure => peer.record_handshake_failure(),
            }
        }

        let mut peers = self.peers.write();
        if let Some(info) = peers.get_mut(overlay) {
            if info.state == PeerState::Connecting {
                info.transition_to(PeerState::Disconnected);
                debug!(?overlay, ?reason, "connection failed");
            }
        }
    }

    /// Ban a peer. They will not be reconnected.
    pub fn ban(&self, overlay: OverlayAddress, reason: Option<String>) {
        warn!(?overlay, ?reason, "banning peer");

        // Remove from pending dials
        self.pending_dials.lock().remove(&overlay);

        // Update or create peer info with banned state
        {
            let mut peers = self.peers.write();
            match peers.get_mut(&overlay) {
                Some(info) => info.ban(reason.clone()),
                None => {
                    let mut info = PeerInfo::new_known();
                    info.ban(reason.clone());
                    peers.insert(overlay, info);
                }
            }
        }

        // Remove from registry if connected
        self.registry.lock().remove_by_overlay(&overlay);

        // Update stored peer
        let key = B256::from(*overlay);
        let needs_persist = {
            let mut stored = self.stored_peers.write();
            if let Some(peer) = stored.get_mut(&key) {
                peer.ban(reason);
                true
            } else {
                false
            }
        };

        if needs_persist {
            if let Some(store) = &self.store {
                if let Some(peer) = self.stored_peers.read().get(&key) {
                    if let Err(e) = store.save(peer) {
                        warn!(?overlay, error = %e, "failed to persist peer ban");
                    }
                }
            }
        }
    }

    /// Add a known peer without connecting.
    ///
    /// Used when we learn about peers from discovery.
    pub fn add_known(&self, overlay: OverlayAddress) {
        let mut peers = self.peers.write();
        peers.entry(overlay).or_insert_with(PeerInfo::new_known);
    }

    /// Check if a dial is pending for this overlay.
    pub fn is_dial_pending(&self, overlay: &OverlayAddress) -> bool {
        self.pending_dials.lock().contains(overlay)
    }

    /// Filter candidates to find peers that are dialable and have cached multiaddrs.
    ///
    /// This is more efficient than calling `is_connected`, `is_dial_pending`, and
    /// `get_multiaddrs` individually for each candidate, as it acquires each lock
    /// only once for the entire batch.
    ///
    /// Returns pairs of (overlay, multiaddrs) for peers that:
    /// - Are not already connected
    /// - Don't have a pending dial
    /// - Have cached multiaddr addresses
    /// - Are in a dialable state (Known or Disconnected)
    pub fn filter_dialable_candidates(
        &self,
        candidates: &[OverlayAddress],
    ) -> Vec<(OverlayAddress, Vec<Multiaddr>)> {
        // Acquire all locks once
        let pending = self.pending_dials.lock();
        let peers = self.peers.read();
        let cache = self.multiaddr_cache.read();

        candidates
            .iter()
            .filter(|overlay| {
                // Skip if dial pending
                if pending.contains(overlay) {
                    return false;
                }
                // Skip if connected or banned
                if let Some(info) = peers.get(overlay) {
                    if !info.state.is_dialable() {
                        return false;
                    }
                }
                true
            })
            .filter_map(|overlay| {
                // Only include if we have cached multiaddrs
                cache.peek(overlay).map(|addrs| (*overlay, addrs.clone()))
            })
            .collect()
    }

    // =========================================================================
    // Persistence API
    // =========================================================================

    /// Add or update a stored peer with full BzzAddress data.
    ///
    /// This is called when we receive peer info from the hive protocol or handshake.
    /// The peer data is stored for later persistence and Hive broadcasting.
    pub fn store_peer(&self, peer: StoredPeer) {
        let overlay = OverlayAddress::from(peer.overlay());

        // Update multiaddr cache
        let multiaddrs = peer.multiaddrs();
        if !multiaddrs.is_empty() {
            self.multiaddr_cache.write().insert(overlay, multiaddrs.to_vec());
        }

        // Ensure peer exists in peers map
        {
            let mut peers = self.peers.write();
            peers.entry(overlay).or_insert_with(|| {
                let mut info = PeerInfo::new_known();
                info.is_full_node = peer.is_full_node;
                info
            });
        }

        // Store the full peer data
        self.stored_peers.write().insert(peer.overlay(), peer.clone());

        // Persist to store if available
        if let Some(store) = &self.store {
            if let Err(e) = store.save(&peer) {
                warn!(?overlay, error = %e, "failed to persist peer to store");
            }
        }
    }

    /// Add or update multiple stored peers in a batch.
    ///
    /// More efficient than calling `store_peer` repeatedly.
    pub fn store_peers_batch(&self, peers: Vec<StoredPeer>) {
        if peers.is_empty() {
            return;
        }

        debug!(count = peers.len(), "storing peers batch");

        // Update multiaddr cache
        {
            let mut cache = self.multiaddr_cache.write();
            for peer in &peers {
                let overlay = OverlayAddress::from(peer.overlay());
                let multiaddrs = peer.multiaddrs();
                if !multiaddrs.is_empty() {
                    cache.insert(overlay, multiaddrs.to_vec());
                }
            }
        }

        // Ensure peers exist in peers map
        {
            let mut peers_map = self.peers.write();
            for peer in &peers {
                let overlay = OverlayAddress::from(peer.overlay());
                peers_map.entry(overlay).or_insert_with(|| {
                    let mut info = PeerInfo::new_known();
                    info.is_full_node = peer.is_full_node;
                    info
                });
            }
        }

        // Store full peer data
        {
            let mut stored = self.stored_peers.write();
            for peer in &peers {
                stored.insert(peer.overlay(), peer.clone());
            }
        }

        // Persist to store if available
        if let Some(store) = &self.store {
            if let Err(e) = store.save_batch(&peers) {
                warn!(error = %e, "failed to persist peers batch to store");
            }
        }
    }

    /// Get a stored peer by overlay address.
    pub fn get_stored_peer(&self, overlay: &OverlayAddress) -> Option<StoredPeer> {
        let key = B256::from(*overlay);
        self.stored_peers.read().get(&key).cloned()
    }

    /// Get all stored peers (for Hive broadcasting).
    pub fn all_stored_peers(&self) -> Vec<StoredPeer> {
        self.stored_peers.read().values().cloned().collect()
    }

    /// Get stored peers that are suitable for Hive broadcast.
    ///
    /// Returns non-banned peers with valid signatures that can be shared.
    pub fn peers_for_hive_broadcast(&self) -> Vec<StoredPeer> {
        self.stored_peers
            .read()
            .values()
            .filter(|p| !p.is_banned())
            .cloned()
            .collect()
    }

    /// Flush all pending changes to the persistent store.
    ///
    /// Call this periodically or before shutdown to ensure data is persisted.
    pub fn flush(&self) -> Result<(), PeerStoreError> {
        if let Some(store) = &self.store {
            store.flush()?;
        }
        Ok(())
    }

    /// Get statistics about the peer manager.
    pub fn stats(&self) -> PeerManagerStats {
        let score_stats = self.scores.stats();
        PeerManagerStats {
            total_peers: self.peers.read().len(),
            connected_peers: self.connected_count(),
            known_peers: self
                .peers
                .read()
                .values()
                .filter(|p| p.state == PeerState::Known)
                .count(),
            banned_peers: self
                .peers
                .read()
                .values()
                .filter(|p| p.state == PeerState::Banned)
                .count(),
            stored_peers: self.stored_peers.read().len(),
            cached_multiaddrs: self.multiaddr_cache.read().len(),
            avg_peer_score: score_stats.avg_peer_score,
            banned_ips: score_stats.banned_ips,
        }
    }

    /// Get a score handle for a peer.
    ///
    /// The handle is cheap to clone and can be stored in per-peer
    /// connection state. Multiple protocol handlers can use the same
    /// handle concurrently without lock contention.
    pub fn score_handle_for(&self, overlay: OverlayAddress) -> ScoreHandle {
        self.scores.handle_for(overlay)
    }

    /// Get the current score for a peer.
    pub fn peer_score(&self, overlay: &OverlayAddress) -> f64 {
        self.scores.get_score(overlay)
    }

    /// Check if a peer should be banned based on score.
    pub fn should_ban_by_score(&self, overlay: &OverlayAddress) -> bool {
        self.scores.should_ban(overlay)
    }

    /// Rank overlays by score (highest first).
    pub fn rank_by_score(&self, overlays: &[OverlayAddress]) -> Vec<(OverlayAddress, f64)> {
        self.scores.rank_overlays(overlays)
    }

    /// Check if an IP is banned.
    pub fn is_ip_banned(&self, ip: &std::net::IpAddr) -> bool {
        self.scores.is_ip_banned(ip)
    }

    /// Ban an IP address.
    pub fn ban_ip(&self, ip: std::net::IpAddr, reason: Option<String>) {
        self.scores.ban_ip(ip, reason);
    }

    /// Associate an IP with a peer (for abuse tracking).
    pub fn associate_peer_ip(&self, overlay: OverlayAddress, ip: std::net::IpAddr) {
        self.scores.associate_ip(overlay, ip);
    }

    /// Get access to the underlying score manager.
    pub fn scores(&self) -> &ScoreManager {
        &self.scores
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
    /// Number of stored peers (with BzzAddress).
    pub stored_peers: usize,
    /// Number of cached multiaddr entries.
    pub cached_multiaddrs: usize,
    /// Average peer score.
    pub avg_peer_score: f64,
    /// Number of banned IPs.
    pub banned_ips: usize,
}

impl Default for PeerManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Bridge trait for operations that require PeerId.
///
/// This trait is implemented by PeerManager and used by vertex-client-core
/// to handle the PeerId ↔ OverlayAddress mapping at the boundary between
/// the libp2p network layer and the Swarm application layer.
///
/// Methods on this trait accept or return PeerId, making the abstraction
/// boundary explicit. Only the bridge layer (SwarmNode) should use this trait.
pub trait InternalPeerManager {
    /// Called when a peer completes handshake.
    ///
    /// Maps the PeerId to OverlayAddress internally and updates state to Connected.
    fn on_peer_ready(&self, peer_id: PeerId, overlay: OverlayAddress, is_full_node: bool);

    /// Called when a peer disconnects.
    ///
    /// Returns the OverlayAddress if the peer was known.
    fn on_peer_disconnected(&self, peer_id: &PeerId) -> Option<OverlayAddress>;

    /// Resolve an OverlayAddress to its PeerId.
    ///
    /// Used when the bridge layer needs to send a command back to topology
    /// (e.g., disconnect a peer).
    fn resolve_peer_id(&self, overlay: &OverlayAddress) -> Option<PeerId>;
}

impl InternalPeerManager for PeerManager {
    fn on_peer_ready(&self, peer_id: PeerId, overlay: OverlayAddress, is_full_node: bool) {
        debug!(?overlay, %peer_id, is_full_node, "peer ready");

        // Remove from pending dials
        self.pending_dials.lock().remove(&overlay);

        // Register the mapping
        self.registry.lock().register(overlay, peer_id);

        // Record connection success in score manager
        self.scores.handle_for(overlay).record_connection_success(0);

        // Update peer state
        {
            let mut peers = self.peers.write();
            match peers.get_mut(&overlay) {
                Some(info) => {
                    info.transition_to(PeerState::Connected);
                    info.is_full_node = is_full_node;
                }
                None => {
                    peers.insert(overlay, PeerInfo::new_connected(is_full_node));
                }
            }
        }

        // Update stored peer stats
        let key = B256::from(*overlay);
        let needs_persist = {
            let mut stored = self.stored_peers.write();
            if let Some(peer) = stored.get_mut(&key) {
                peer.record_connection();
                peer.is_full_node = is_full_node;
                true
            } else {
                false
            }
        };

        if needs_persist {
            if let Some(store) = &self.store {
                if let Some(peer) = self.stored_peers.read().get(&key) {
                    if let Err(e) = store.save(peer) {
                        warn!(?overlay, error = %e, "failed to persist peer connection");
                    }
                }
            }
        }
    }

    fn on_peer_disconnected(&self, peer_id: &PeerId) -> Option<OverlayAddress> {
        let overlay = self.registry.lock().remove_by_peer(peer_id)?;

        debug!(?overlay, %peer_id, "peer disconnected");

        let mut peers = self.peers.write();
        if let Some(info) = peers.get_mut(&overlay) {
            info.transition_to(PeerState::Disconnected);
        }

        Some(overlay)
    }

    fn resolve_peer_id(&self, overlay: &OverlayAddress) -> Option<PeerId> {
        self.registry.lock().resolve_peer(overlay)
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

    #[test]
    fn test_peer_lifecycle() {
        let pm = PeerManager::new();
        let overlay = test_overlay(1);
        let peer_id = test_peer_id(1);

        // Initially unknown
        assert!(pm.state(&overlay).is_none());
        assert!(!pm.is_connected(&overlay));

        // Add as known
        pm.add_known(overlay);
        assert_eq!(pm.state(&overlay), Some(PeerState::Known));

        // Start connecting
        assert!(pm.start_connecting(overlay));
        assert_eq!(pm.state(&overlay), Some(PeerState::Connecting));
        assert!(pm.is_dial_pending(&overlay));

        // Can't start connecting again
        assert!(!pm.start_connecting(overlay));

        // Peer connects
        pm.on_peer_ready(peer_id, overlay, true);
        assert_eq!(pm.state(&overlay), Some(PeerState::Connected));
        assert!(pm.is_connected(&overlay));
        assert!(!pm.is_dial_pending(&overlay));
        assert_eq!(pm.connected_count(), 1);

        // Resolve peer_id
        assert_eq!(pm.resolve_peer_id(&overlay), Some(peer_id));

        // Disconnect
        let disconnected = pm.on_peer_disconnected(&peer_id);
        assert_eq!(disconnected, Some(overlay));
        assert_eq!(pm.state(&overlay), Some(PeerState::Disconnected));
        assert!(!pm.is_connected(&overlay));
        assert_eq!(pm.connected_count(), 0);
    }

    #[test]
    fn test_connection_failure() {
        let pm = PeerManager::new();
        let overlay = test_overlay(1);

        pm.add_known(overlay);
        assert!(pm.start_connecting(overlay));

        pm.connection_failed(&overlay);
        assert_eq!(pm.state(&overlay), Some(PeerState::Disconnected));
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

        assert_eq!(pm.state(&overlay), Some(PeerState::Banned));
        assert!(!pm.start_connecting(overlay)); // Can't dial banned peer
        assert!(pm.resolve_peer_id(&overlay).is_none()); // Mapping removed
    }

    #[test]
    fn test_multiaddr_cache() {
        let pm = PeerManager::new();
        let overlay = test_overlay(1);
        let addrs: Vec<Multiaddr> = vec!["/ip4/127.0.0.1/tcp/1234".parse().unwrap()];

        pm.cache_multiaddrs(overlay, addrs.clone());

        // Should create Known peer
        assert_eq!(pm.state(&overlay), Some(PeerState::Known));
        assert_eq!(pm.get_multiaddrs(&overlay), Some(addrs));
    }

    #[test]
    fn test_cache_multiaddrs_batch() {
        let pm = PeerManager::new();

        let entries: Vec<_> = (1..=5)
            .map(|n| {
                let overlay = test_overlay(n);
                let addrs: Vec<Multiaddr> =
                    vec![format!("/ip4/127.0.0.{}/tcp/1234", n).parse().unwrap()];
                (overlay, addrs)
            })
            .collect();

        pm.cache_multiaddrs_batch(entries.clone());

        // All should be known with cached multiaddrs
        for (overlay, addrs) in entries {
            assert_eq!(pm.state(&overlay), Some(PeerState::Known));
            assert_eq!(pm.get_multiaddrs(&overlay), Some(addrs));
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
        pm.cache_multiaddrs(overlays[0], vec![addr1.clone()]);

        // Peer 2: Connected (not dialable)
        pm.cache_multiaddrs(
            overlays[1],
            vec!["/ip4/127.0.0.2/tcp/1234".parse().unwrap()],
        );
        pm.on_peer_ready(peer_ids[1], overlays[1], false);

        // Peer 3: Dial pending (not dialable)
        pm.cache_multiaddrs(
            overlays[2],
            vec!["/ip4/127.0.0.3/tcp/1234".parse().unwrap()],
        );
        pm.start_connecting(overlays[2]);

        // Peer 4: Banned (not dialable)
        pm.cache_multiaddrs(
            overlays[3],
            vec!["/ip4/127.0.0.4/tcp/1234".parse().unwrap()],
        );
        pm.ban(overlays[3], None);

        // Peer 5: Known but no multiaddrs cached (not returned)
        pm.add_known(overlays[4]);

        // Filter candidates
        let dialable = pm.filter_dialable_candidates(&overlays);

        // Only peer 1 should be dialable
        assert_eq!(dialable.len(), 1);
        assert_eq!(dialable[0].0, overlays[0]);
        assert_eq!(dialable[0].1, vec![addr1]);
    }
}
