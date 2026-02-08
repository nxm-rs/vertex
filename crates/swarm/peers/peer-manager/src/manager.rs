//! Peer manager with Arc-per-peer pattern.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use tracing::{debug, trace, warn};
use vertex_net_local::IpCapability;
use vertex_net_peer_store::{FilePeerStore, NetPeerStore, PeerRecord, StoreError};
use vertex_swarm_api::PeerConfigValues;
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_peer_score::SwarmScoringConfig;
use vertex_swarm_primitives::OverlayAddress;

use crate::data::SwarmPeerData;
use crate::entry::PeerEntry;
use crate::error::PeerManagerError;
use crate::snapshot::SwarmPeerSnapshot;

/// Default maximum tracked peers.
pub const DEFAULT_MAX_TRACKED_PEERS: usize = 10_000;

/// Peer lifecycle manager with Arc-per-peer pattern.
///
/// Stores peer metadata (scoring, addresses, ban info). Connection state and
/// PeerId ↔ OverlayAddress mapping is tracked by ConnectionRegistry in the topology layer.
pub struct PeerManager {
    peers: RwLock<HashMap<OverlayAddress, Arc<PeerEntry>>>,
    store: Option<Arc<dyn NetPeerStore<OverlayAddress, SwarmPeerSnapshot>>>,
    scoring_config: Arc<SwarmScoringConfig>,
    max_tracked_peers: Option<usize>,
}

impl PeerManager {
    /// Create a new peer manager with default settings.
    pub fn new() -> Self {
        Self::with_config(SwarmScoringConfig::default(), Some(DEFAULT_MAX_TRACKED_PEERS))
    }

    /// Create with specified scoring config and limits.
    pub fn with_config(scoring_config: SwarmScoringConfig, max_tracked_peers: Option<usize>) -> Self {
        Self {
            peers: RwLock::new(HashMap::new()),
            store: None,
            scoring_config: Arc::new(scoring_config),
            max_tracked_peers,
        }
    }

    /// Create with specified limits (uses default scoring config).
    pub fn with_limits(ban_threshold: f64, max_tracked_peers: Option<usize>) -> Self {
        let mut config = SwarmScoringConfig::default();
        config.ban_threshold = ban_threshold;
        Self::with_config(config, max_tracked_peers)
    }

    /// Create with a peer store for persistence.
    pub fn with_store(
        store: Arc<dyn NetPeerStore<OverlayAddress, SwarmPeerSnapshot>>,
    ) -> Result<Self, StoreError> {
        Self::with_store_and_config(
            store,
            SwarmScoringConfig::default(),
            Some(DEFAULT_MAX_TRACKED_PEERS),
        )
    }

    /// Create with store and specified config.
    pub fn with_store_and_config(
        store: Arc<dyn NetPeerStore<OverlayAddress, SwarmPeerSnapshot>>,
        scoring_config: SwarmScoringConfig,
        max_tracked_peers: Option<usize>,
    ) -> Result<Self, StoreError> {
        let mut pm = Self::with_config(scoring_config, max_tracked_peers);
        pm.store = Some(store);
        pm.load_from_store()?;
        Ok(pm)
    }

    /// Create with store and specified limits (legacy API).
    pub fn with_store_and_limits(
        store: Arc<dyn NetPeerStore<OverlayAddress, SwarmPeerSnapshot>>,
        ban_threshold: f64,
        max_tracked_peers: Option<usize>,
    ) -> Result<Self, StoreError> {
        let mut config = SwarmScoringConfig::default();
        config.ban_threshold = ban_threshold;
        Self::with_store_and_config(store, config, max_tracked_peers)
    }

    /// Create from configuration.
    pub fn from_config(config: &impl PeerConfigValues) -> Result<Self, PeerManagerError> {
        let mut scoring_config = SwarmScoringConfig::default();
        scoring_config.ban_threshold = config.ban_threshold();
        let max_peers = config.store_limit();

        match config.store_path() {
            Some(path) => {
                let store = FilePeerStore::new_with_create_dir(&path)?;
                let pm = Self::with_store_and_config(Arc::new(store), scoring_config, max_peers)?;
                tracing::info!(
                    count = pm.stats().total_peers,
                    path = %path.display(),
                    "loaded peers from store"
                );
                Ok(pm)
            }
            None => Ok(Self::with_config(scoring_config, max_peers)),
        }
    }

    /// Get the scoring configuration.
    pub fn scoring_config(&self) -> &SwarmScoringConfig {
        &self.scoring_config
    }

    /// Get the ban threshold.
    pub fn ban_threshold(&self) -> f64 {
        self.scoring_config.ban_threshold
    }

    fn load_from_store(&self) -> Result<(), StoreError> {
        let Some(store) = &self.store else {
            return Ok(());
        };

        let records = store.load_all()?;
        let count = records.len();

        if count > 0 {
            let mut peers = self.peers.write();
            for record in records {
                let overlay = *record.id();
                let snapshot = record.into_data();
                let entry = Arc::new(PeerEntry::from_snapshot_with_config(
                    snapshot,
                    Arc::clone(&self.scoring_config),
                ));
                peers.insert(overlay, entry);
            }
            tracing::info!(count, "loaded peers from store");
        }
        Ok(())
    }

    /// Check if a peer is a full node.
    pub fn is_full_node(&self, overlay: &OverlayAddress) -> bool {
        self.peers
            .read()
            .get(overlay)
            .map(|e| e.is_full_node())
            .unwrap_or(false)
    }

    /// Get known peers that are not banned (for seeding routing tables).
    pub fn known_peers(&self) -> Vec<OverlayAddress> {
        self.peers
            .read()
            .iter()
            .filter(|(_, e)| !e.is_banned())
            .map(|(o, _)| *o)
            .collect()
    }

    /// Count of known peers that are not banned (avoids allocating Vec).
    pub fn known_peers_count(&self) -> usize {
        self.peers
            .read()
            .iter()
            .filter(|(_, e)| !e.is_banned())
            .count()
    }

    /// Get SwarmPeers for candidates that are not banned.
    ///
    /// Used by topology to get dialable peers. Connection state filtering
    /// should be done by the caller using ConnectionRegistry.
    pub fn get_dialable_peers(&self, candidates: &[OverlayAddress]) -> Vec<SwarmPeer> {
        let peers = self.peers.read();
        candidates
            .iter()
            .filter_map(|overlay| {
                let entry = peers.get(overlay)?;
                if entry.is_banned() {
                    return None;
                }
                Some(entry.swarm_peer())
            })
            .collect()
    }

    /// Get multiaddrs for a peer.
    pub fn get_multiaddrs(&self, overlay: &OverlayAddress) -> Option<Vec<libp2p::Multiaddr>> {
        self.peers
            .read()
            .get(overlay)
            .map(|e| e.swarm_peer().multiaddrs().to_vec())
    }

    /// Get a peer's IP capability.
    pub fn get_peer_capability(&self, overlay: &OverlayAddress) -> Option<IpCapability> {
        self.peers.read().get(overlay).map(|e| e.ip_capability())
    }

    /// Get a peer entry by overlay address.
    pub fn get_peer(&self, overlay: &OverlayAddress) -> Option<Arc<PeerEntry>> {
        self.peers.read().get(overlay).cloned()
    }

    /// Store a single discovered peer.
    pub fn store_discovered_peer(&self, swarm_peer: SwarmPeer) -> OverlayAddress {
        let overlay = OverlayAddress::from(*swarm_peer.overlay());
        let data = SwarmPeerData::new(swarm_peer, false);
        let entry = self.insert_peer(overlay, data);
        self.persist_peer(&overlay, &entry);
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
            let data = SwarmPeerData::new(swarm_peer, false);
            let entry = self.insert_peer(overlay, data);

            to_persist.push((overlay, entry));
            stored_overlays.push(overlay);
        }

        self.persist_batch(&to_persist);
        stored_overlays
    }

    /// Get a peer snapshot by overlay address.
    pub fn get_peer_snapshot(&self, overlay: &OverlayAddress) -> Option<SwarmPeerSnapshot> {
        self.peers.read().get(overlay).map(|e| e.snapshot())
    }

    /// Check if a peer is banned.
    pub fn is_banned(&self, overlay: &OverlayAddress) -> bool {
        self.peers
            .read()
            .get(overlay)
            .map(|e| e.is_banned())
            .unwrap_or(false)
    }

    /// Ban a peer.
    pub fn ban(&self, overlay: &OverlayAddress, reason: Option<String>) {
        warn!(?overlay, ?reason, "banning peer");
        if let Some(entry) = self.peers.read().get(overlay) {
            entry.ban(reason);
            self.persist_peer(overlay, entry);
        }
    }

    /// Insert or update a peer, returns the entry.
    fn insert_peer(&self, overlay: OverlayAddress, data: SwarmPeerData) -> Arc<PeerEntry> {
        let mut peers = self.peers.write();

        if let Some(existing) = peers.get(&overlay) {
            existing.update_data(data);
            return Arc::clone(existing);
        }

        // Check if we need to prune
        if let Some(max) = self.max_tracked_peers
            && peers.len() >= max
        {
            self.prune_peers_locked(&mut peers);
        }

        let entry = Arc::new(PeerEntry::with_config(data, Arc::clone(&self.scoring_config)));
        peers.insert(overlay, Arc::clone(&entry));
        entry
    }

    /// Prune peers when at capacity: banned first, then lowest score, then oldest.
    fn prune_peers_locked(&self, peers: &mut HashMap<OverlayAddress, Arc<PeerEntry>>) {
        let Some(max) = self.max_tracked_peers else {
            return;
        };

        let to_remove = peers.len().saturating_sub(max / 2);
        if to_remove == 0 {
            return;
        }

        let ban_threshold = self.scoring_config.ban_threshold;

        // Collect peers with their pruning priority
        let mut candidates: Vec<_> = peers
            .iter()
            .map(|(o, e)| {
                let priority = if e.is_banned() {
                    0 // Banned peers removed first
                } else if e.score() < ban_threshold {
                    1 // Low score peers next
                } else {
                    2 // Normal peers last
                };
                (*o, priority, e.score(), e.last_seen())
            })
            .collect();

        // Sort by: priority (ascending), score (ascending), last_seen (ascending)
        candidates.sort_by(|a, b| {
            a.1.cmp(&b.1)
                .then(a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
                .then(a.3.cmp(&b.3))
        });

        // Remove the first `to_remove` peers
        for (overlay, _, _, _) in candidates.into_iter().take(to_remove) {
            peers.remove(&overlay);
            if let Some(store) = &self.store {
                let _ = store.remove(&overlay);
            }
        }

        debug!(removed = to_remove, remaining = peers.len(), "pruned peers");
    }

    fn persist_peer(&self, overlay: &OverlayAddress, entry: &PeerEntry) {
        let Some(store) = &self.store else {
            return;
        };
        let snapshot = entry.snapshot();
        let record = PeerRecord::new(*overlay, snapshot, entry.first_seen(), entry.last_seen());
        if let Err(e) = store.save(&record) {
            warn!(?overlay, error = %e, "failed to persist peer");
        }
    }

    fn persist_batch(&self, entries: &[(OverlayAddress, Arc<PeerEntry>)]) {
        let Some(store) = &self.store else {
            return;
        };
        let records: Vec<_> = entries
            .iter()
            .map(|(overlay, entry)| {
                let snapshot = entry.snapshot();
                PeerRecord::new(*overlay, snapshot, entry.first_seen(), entry.last_seen())
            })
            .collect();
        if let Err(e) = store.save_batch(&records) {
            warn!(error = %e, "failed to persist peers batch");
        }
    }

    fn save_all_to_store(&self) -> Result<usize, StoreError> {
        let Some(store) = &self.store else {
            return Ok(0);
        };
        let peers = self.peers.read();
        let records: Vec<_> = peers
            .iter()
            .map(|(overlay, entry)| {
                let snapshot = entry.snapshot();
                PeerRecord::new(*overlay, snapshot, entry.first_seen(), entry.last_seen())
            })
            .collect();
        let count = records.len();
        store.save_batch(&records)?;
        store.flush()?;
        Ok(count)
    }

    /// Get statistics about the peer manager.
    pub fn stats(&self) -> PeerManagerStats {
        let peers = self.peers.read();
        let total = peers.len();

        let mut banned = 0;
        let mut total_score = 0.0;

        for entry in peers.values() {
            if entry.is_banned() {
                banned += 1;
            }
            total_score += entry.score();
        }

        let avg_score = if total > 0 {
            total_score / total as f64
        } else {
            0.0
        };

        PeerManagerStats {
            total_peers: total,
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
    pub banned_peers: usize,
    pub avg_peer_score: f64,
}

/// Bridge trait for peer manager operations from topology layer.
pub trait InternalPeerManager: Send + Sync {
    /// Called when a peer completes handshake. Stores the SwarmPeer metadata.
    fn on_peer_ready(&self, swarm_peer: SwarmPeer, is_full_node: bool);

    /// Record latency for a peer.
    fn record_latency(&self, overlay: &OverlayAddress, rtt: Duration);
}

impl InternalPeerManager for PeerManager {
    fn on_peer_ready(&self, swarm_peer: SwarmPeer, is_full_node: bool) {
        let overlay = OverlayAddress::from(*swarm_peer.overlay());
        debug!(?overlay, is_full_node, "storing peer");

        let data = SwarmPeerData::new(swarm_peer, is_full_node);
        let entry = self.insert_peer(overlay, data);
        entry.record_success(Duration::ZERO);
        self.persist_peer(&overlay, &entry);
    }

    fn record_latency(&self, overlay: &OverlayAddress, rtt: Duration) {
        if let Some(entry) = self.peers.read().get(overlay) {
            entry.set_latency(rtt);
            trace!(?overlay, ?rtt, "recorded latency");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_test_utils::{test_overlay, test_swarm_peer};

    #[test]
    fn test_store_discovered_peer() {
        let pm = PeerManager::new();
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        let stored = pm.store_discovered_peer(swarm_peer.clone());
        assert_eq!(stored, overlay);
        assert!(pm.get_multiaddrs(&overlay).is_some());
    }

    #[test]
    fn test_on_peer_ready() {
        let pm = PeerManager::new();
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        pm.on_peer_ready(swarm_peer, true);
        assert!(pm.is_full_node(&overlay));
    }

    #[test]
    fn test_peer_lifecycle() {
        let pm = PeerManager::new();
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        // Discover peer via Hive
        pm.store_discovered_peer(swarm_peer.clone());

        // Should be in known peers list
        assert!(pm.known_peers().contains(&overlay));

        // Store as connected peer
        pm.on_peer_ready(swarm_peer, false);

        // Still in known peers
        assert!(pm.known_peers().contains(&overlay));
    }

    #[test]
    fn test_ban() {
        let pm = PeerManager::new();
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        pm.on_peer_ready(swarm_peer, false);
        pm.ban(&overlay, Some("misbehaving".to_string()));

        assert!(pm.is_banned(&overlay));
        // Banned peers should not appear in known_peers
        assert!(!pm.known_peers().contains(&overlay));
    }

    #[test]
    fn test_get_dialable_peers() {
        let pm = PeerManager::new();

        // Store some peers
        for n in 1..=5 {
            pm.store_discovered_peer(test_swarm_peer(n));
        }

        // Ban one
        pm.ban(&test_overlay(1), None);

        // Get dialable peers - should exclude banned
        let all_overlays: Vec<_> = (1..=5).map(test_overlay).collect();
        let dialable = pm.get_dialable_peers(&all_overlays);

        assert_eq!(dialable.len(), 4);
    }

    #[test]
    fn test_pruning() {
        let pm = PeerManager::with_limits(-100.0, Some(10));

        // Add more peers than max
        for n in 1..=15 {
            pm.store_discovered_peer(test_swarm_peer(n));
        }

        // Should have pruned down
        assert!(pm.stats().total_peers <= 10);
    }

    #[test]
    fn test_custom_scoring_config() {
        let config = SwarmScoringConfig::lenient();
        let pm = PeerManager::with_config(config.clone(), None);

        assert_eq!(pm.scoring_config().connection_timeout, config.connection_timeout);
    }
}
