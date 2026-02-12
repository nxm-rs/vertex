//! Peer manager with Arc-per-peer pattern and DashMap for concurrent access.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use tokio::sync::oneshot;
use tracing::{debug, trace, warn};
use vertex_net_local::IpCapability;
use vertex_net_peer_store::{FilePeerStore, NetPeerStore, PeerRecord, StoreError};
use vertex_swarm_api::PeerConfigValues;
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_peer_score::SwarmScoringConfig;
use vertex_swarm_primitives::{OverlayAddress, SwarmNodeType};
use vertex_tasks::TaskExecutor;

use crate::entry::PeerEntry;
use crate::error::PeerManagerError;
use crate::pruner::PruneConfig;
use crate::snapshot::SwarmPeerSnapshot;

/// Default maximum tracked peers.
pub const DEFAULT_MAX_TRACKED_PEERS: usize = 10_000;

/// Peer lifecycle manager with Arc-per-peer pattern.
///
/// Uses DashMap for concurrent peer access without global locks. Connection state
/// and PeerId ↔ OverlayAddress mapping is tracked by ConnectionRegistry in the topology layer.
pub struct PeerManager {
    peers: DashMap<OverlayAddress, Arc<PeerEntry>>,
    store: Option<Arc<dyn NetPeerStore<OverlayAddress, SwarmPeerSnapshot>>>,
    scoring_config: Arc<SwarmScoringConfig>,
    max_tracked_peers: Option<usize>,
    prune_in_progress: AtomicBool,
}

impl PeerManager {
    /// Create with default settings.
    pub fn new() -> Self {
        Self::with_config(SwarmScoringConfig::default(), Some(DEFAULT_MAX_TRACKED_PEERS))
    }

    /// Create with specified scoring config and limits.
    pub fn with_config(scoring_config: SwarmScoringConfig, max_tracked_peers: Option<usize>) -> Self {
        Self {
            peers: DashMap::new(),
            store: None,
            scoring_config: Arc::new(scoring_config),
            max_tracked_peers,
            prune_in_progress: AtomicBool::new(false),
        }
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

    /// Create from configuration.
    pub fn from_config(config: &impl PeerConfigValues) -> Result<Self, PeerManagerError> {
        let scoring_config = SwarmScoringConfig::builder()
            .ban_threshold(config.ban_threshold())
            .build();
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

    pub fn scoring_config(&self) -> &SwarmScoringConfig {
        &self.scoring_config
    }

    pub fn ban_threshold(&self) -> f64 {
        self.scoring_config.ban_threshold()
    }

    pub fn max_tracked_peers(&self) -> Option<usize> {
        self.max_tracked_peers
    }

    fn load_from_store(&self) -> Result<(), StoreError> {
        let Some(store) = &self.store else {
            return Ok(());
        };

        let records = store.load_all()?;
        let count = records.len();

        if count > 0 {
            for record in records {
                let overlay = *record.id();
                let snapshot = record.into_data();
                let entry = Arc::new(PeerEntry::from_snapshot_with_config(
                    snapshot,
                    Arc::clone(&self.scoring_config),
                ));
                self.peers.insert(overlay, entry);
            }
            tracing::info!(count, "loaded peers from store");
        }
        Ok(())
    }

    pub fn node_type(&self, overlay: &OverlayAddress) -> Option<SwarmNodeType> {
        self.peers.get(overlay).map(|e| e.node_type())
    }

    /// Get known peers that are not banned (for seeding routing tables).
    #[must_use]
    pub fn known_peers(&self) -> Vec<OverlayAddress> {
        self.peers
            .iter()
            .filter(|r| !r.value().is_banned())
            .map(|r| *r.key())
            .collect()
    }

    /// Count of known peers that are not banned.
    pub fn known_peers_count(&self) -> usize {
        self.peers
            .iter()
            .filter(|r| !r.value().is_banned())
            .count()
    }

    /// Get all known Storer peers that aren't banned.
    #[must_use]
    pub fn known_storer_overlays(&self) -> Vec<OverlayAddress> {
        self.peers
            .iter()
            .filter(|r| r.value().node_type() == SwarmNodeType::Storer && !r.value().is_banned())
            .map(|r| *r.key())
            .collect()
    }

    /// Get SwarmPeer data for multiple overlays.
    #[must_use]
    pub fn get_swarm_peers(&self, overlays: &[OverlayAddress]) -> Vec<SwarmPeer> {
        overlays
            .iter()
            .filter_map(|o| self.peers.get(o).map(|r| r.swarm_peer()))
            .collect()
    }

    /// Get SwarmPeers for candidates that are not banned and not in backoff.
    ///
    /// Used by topology to get dialable peers. Connection state filtering
    /// should be done by the caller using ConnectionRegistry.
    #[must_use]
    pub fn get_dialable_peers(&self, candidates: &[OverlayAddress]) -> Vec<SwarmPeer> {
        candidates
            .iter()
            .filter_map(|overlay| {
                let entry = self.peers.get(overlay)?;
                if entry.is_banned() {
                    return None;
                }
                if entry.is_in_backoff() {
                    trace!(?overlay, backoff = ?entry.backoff_remaining(), "peer in backoff");
                    return None;
                }
                Some(entry.swarm_peer())
            })
            .collect()
    }

    #[must_use]
    pub fn get_multiaddrs(&self, overlay: &OverlayAddress) -> Option<Vec<libp2p::Multiaddr>> {
        self.peers
            .get(overlay)
            .map(|r| r.swarm_peer().multiaddrs().to_vec())
    }

    #[must_use]
    pub fn get_peer_capability(&self, overlay: &OverlayAddress) -> Option<IpCapability> {
        self.peers.get(overlay).map(|r| r.ip_capability())
    }

    #[must_use]
    pub fn get_peer_score(&self, overlay: &OverlayAddress) -> Option<f64> {
        self.peers.get(overlay).map(|r| r.score())
    }

    #[must_use]
    pub fn get_peer_latency(&self, overlay: &OverlayAddress) -> Option<Duration> {
        self.peers.get(overlay).and_then(|r| r.latency())
    }

    pub fn peer_is_in_backoff(&self, overlay: &OverlayAddress) -> bool {
        self.peers.get(overlay).is_some_and(|r| r.is_in_backoff())
    }

    pub fn peer_backoff_remaining(&self, overlay: &OverlayAddress) -> Option<Duration> {
        self.peers.get(overlay).and_then(|r| r.backoff_remaining())
    }

    /// Store a single discovered peer (default to Client node type).
    pub fn store_discovered_peer(&self, swarm_peer: SwarmPeer) -> OverlayAddress {
        let overlay = OverlayAddress::from(*swarm_peer.overlay());
        let entry = self.insert_peer(overlay, swarm_peer, SwarmNodeType::Client);
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
            let entry = self.insert_peer(overlay, swarm_peer, SwarmNodeType::Client);

            to_persist.push((overlay, entry));
            stored_overlays.push(overlay);
        }

        self.persist_batch(&to_persist);
        stored_overlays
    }

    #[must_use]
    pub fn get_peer_snapshot(&self, overlay: &OverlayAddress) -> Option<SwarmPeerSnapshot> {
        self.peers.get(overlay).map(|r| r.snapshot())
    }

    pub fn is_banned(&self, overlay: &OverlayAddress) -> bool {
        self.peers.get(overlay).is_some_and(|r| r.is_banned())
    }

    /// Ban a peer (prevents dialing, persists to store).
    pub fn ban(&self, overlay: &OverlayAddress, reason: Option<String>) {
        warn!(?overlay, ?reason, "banning peer");
        if let Some(entry) = self.peers.get(overlay) {
            entry.ban(reason);
            self.persist_peer(overlay, &entry);
        }
    }

    /// Insert or update a peer, returns the entry.
    fn insert_peer(&self, overlay: OverlayAddress, peer: SwarmPeer, node_type: SwarmNodeType) -> Arc<PeerEntry> {
        use dashmap::mapref::entry::Entry;

        match self.peers.entry(overlay) {
            Entry::Occupied(e) => {
                e.get().update_peer(peer);
                Arc::clone(e.get())
            }
            Entry::Vacant(e) => {
                let entry = Arc::new(PeerEntry::with_config(peer, node_type, Arc::clone(&self.scoring_config)));
                e.insert(Arc::clone(&entry));
                entry
            }
        }
    }

    /// Returns true if peer count exceeds capacity threshold and no prune in progress.
    #[must_use]
    pub fn should_prune(&self, config: &PruneConfig) -> bool {
        let Some(max) = self.max_tracked_peers else {
            return false;
        };
        if self.prune_in_progress.load(Ordering::Acquire) {
            return false;
        }
        self.peers.len() >= (max as f64 * config.capacity_threshold) as usize
    }

    /// Remove low-priority peers (banned, stale, low-score) until target utilization reached.
    pub async fn prune_async(&self, config: &PruneConfig) {
        if self
            .prune_in_progress
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        let _guard = scopeguard::guard((), |_| {
            self.prune_in_progress.store(false, Ordering::Release);
        });

        let Some(max) = self.max_tracked_peers else {
            return;
        };

        let target = (max as f64 * config.target_utilization) as usize;
        let to_remove = self.peers.len().saturating_sub(target);

        if to_remove == 0 {
            return;
        }

        let candidates = self.collect_prune_candidates(to_remove).await;

        for batch in candidates.chunks(config.batch_size) {
            for overlay in batch {
                self.peers.remove(overlay);
                if let Some(store) = &self.store {
                    let _ = store.remove(overlay);
                }
            }
            tokio::task::yield_now().await;
        }

        debug!(removed = candidates.len(), remaining = self.peers.len(), "pruned peers");
    }

    /// Collect and sort prune candidates. Offloads sort to blocking thread pool via TaskExecutor.
    async fn collect_prune_candidates(&self, count: usize) -> Vec<OverlayAddress> {
        let ban_threshold = self.scoring_config.ban_threshold();

        // Collect candidate data from DashMap
        let candidates: Vec<_> = self
            .peers
            .iter()
            .map(|r| {
                let e = r.value();
                let priority = if e.is_banned() {
                    0 // Banned peers removed first
                } else if e.is_stale() {
                    1 // Stale peers (no connection in 1 week with failures)
                } else if e.score() < ban_threshold {
                    2 // Low score peers next
                } else {
                    3 // Normal peers last
                };
                (*r.key(), priority, e.score(), e.last_seen())
            })
            .collect();

        // Offload CPU-intensive sort to blocking thread pool via TaskExecutor
        let Ok(executor) = TaskExecutor::try_current() else {
            // No executor available, sort synchronously
            return Self::sort_candidates(candidates, count);
        };

        let (tx, rx) = oneshot::channel();
        executor.spawn_blocking(async move {
            let result = Self::sort_candidates(candidates, count);
            let _ = tx.send(result);
        });

        rx.await.unwrap_or_default()
    }

    fn sort_candidates(
        mut candidates: Vec<(OverlayAddress, u8, f64, u64)>,
        count: usize,
    ) -> Vec<OverlayAddress> {
        candidates.sort_unstable_by(|a, b| {
            a.1.cmp(&b.1)
                .then(a.2.total_cmp(&b.2))
                .then(a.3.cmp(&b.3))
        });
        candidates.into_iter().take(count).map(|(o, ..)| o).collect()
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

        let records: Vec<_> = self
            .peers
            .iter()
            .map(|r| {
                let snapshot = r.value().snapshot();
                PeerRecord::new(*r.key(), snapshot, r.value().first_seen(), r.value().last_seen())
            })
            .collect();

        let count = records.len();
        store.save_batch(&records)?;
        store.flush()?;
        Ok(count)
    }

    /// Get statistics about the peer manager.
    #[must_use]
    pub fn stats(&self) -> PeerManagerStats {
        let total = self.peers.len();

        let mut banned = 0;
        let mut total_score = 0.0;

        for r in self.peers.iter() {
            if r.value().is_banned() {
                banned += 1;
            }
            total_score += r.value().score();
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

impl PeerManager {
    /// Called when a peer completes handshake. Stores the SwarmPeer metadata.
    pub fn on_peer_ready(&self, swarm_peer: SwarmPeer, node_type: SwarmNodeType) {
        let overlay = OverlayAddress::from(*swarm_peer.overlay());
        debug!(?overlay, ?node_type, "storing peer");

        let entry = self.insert_peer(overlay, swarm_peer, node_type);
        entry.record_success(Duration::ZERO);
        self.persist_peer(&overlay, &entry);
    }

    pub fn record_latency(&self, overlay: &OverlayAddress, rtt: Duration) {
        if let Some(entry) = self.peers.get(overlay) {
            entry.set_latency(rtt);
            trace!(?overlay, ?rtt, "recorded latency");
        }
    }

    pub fn record_dial_failure(&self, overlay: &OverlayAddress) {
        if let Some(entry) = self.peers.get(overlay) {
            entry.record_dial_failure();
            let failures = entry.consecutive_failures();
            let backoff = entry.backoff_remaining();
            debug!(
                ?overlay,
                failures,
                backoff_secs = backoff.map(|d| d.as_secs()),
                "recorded dial failure with backoff"
            );
            self.persist_peer(overlay, &entry);
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

        pm.on_peer_ready(swarm_peer, SwarmNodeType::Storer);
        assert_eq!(pm.node_type(&overlay), Some(SwarmNodeType::Storer));
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
        pm.on_peer_ready(swarm_peer, SwarmNodeType::Client);

        // Still in known peers
        assert!(pm.known_peers().contains(&overlay));
    }

    #[test]
    fn test_ban() {
        let pm = PeerManager::new();
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        pm.on_peer_ready(swarm_peer, SwarmNodeType::Client);
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

    #[tokio::test]
    async fn test_async_pruning() {
        let pm = PeerManager::with_config(SwarmScoringConfig::default(), Some(10));

        // Add more peers than max
        for n in 1..=15 {
            pm.store_discovered_peer(test_swarm_peer(n));
        }

        // Should have all peers (no sync pruning)
        assert_eq!(pm.stats().total_peers, 15);

        // Now prune async
        let config = PruneConfig {
            capacity_threshold: 0.5,
            target_utilization: 0.5,
            batch_size: 5,
            ..Default::default()
        };
        pm.prune_async(&config).await;

        // Should have pruned down to target
        assert!(pm.stats().total_peers <= 5);
    }

    #[test]
    fn test_custom_scoring_config() {
        let config = SwarmScoringConfig::lenient();
        let pm = PeerManager::with_config(config.clone(), None);

        assert_eq!(pm.scoring_config().connection_timeout(), config.connection_timeout());
    }

    #[test]
    fn test_known_storer_overlays() {
        let pm = PeerManager::new();

        // Store peers with different node types
        pm.on_peer_ready(test_swarm_peer(1), SwarmNodeType::Storer);
        pm.on_peer_ready(test_swarm_peer(2), SwarmNodeType::Client);
        pm.on_peer_ready(test_swarm_peer(3), SwarmNodeType::Storer);
        pm.on_peer_ready(test_swarm_peer(4), SwarmNodeType::Client);

        // Ban one storer
        pm.ban(&test_overlay(1), None);

        let storers = pm.known_storer_overlays();

        // Should only have one non-banned storer (#3)
        assert_eq!(storers.len(), 1);
        assert!(storers.contains(&test_overlay(3)));
    }

    #[test]
    fn test_get_swarm_peers() {
        let pm = PeerManager::new();

        // Store some peers
        for n in 1..=5 {
            pm.on_peer_ready(test_swarm_peer(n), SwarmNodeType::Storer);
        }

        // Request subset of overlays
        let overlays = vec![test_overlay(1), test_overlay(3), test_overlay(5)];
        let peers = pm.get_swarm_peers(&overlays);

        assert_eq!(peers.len(), 3);
    }

    #[test]
    fn test_get_swarm_peers_missing() {
        let pm = PeerManager::new();

        // Store only peer 1
        pm.on_peer_ready(test_swarm_peer(1), SwarmNodeType::Storer);

        // Request overlays including non-existent ones
        let overlays = vec![test_overlay(1), test_overlay(99)];
        let peers = pm.get_swarm_peers(&overlays);

        // Should only return the existing peer
        assert_eq!(peers.len(), 1);
    }

    #[test]
    fn test_node_type_variants() {
        let pm = PeerManager::new();

        pm.on_peer_ready(test_swarm_peer(1), SwarmNodeType::Bootnode);
        pm.on_peer_ready(test_swarm_peer(2), SwarmNodeType::Client);
        pm.on_peer_ready(test_swarm_peer(3), SwarmNodeType::Storer);

        // Check explicit node types
        assert_eq!(pm.node_type(&test_overlay(1)), Some(SwarmNodeType::Bootnode));
        assert_eq!(pm.node_type(&test_overlay(2)), Some(SwarmNodeType::Client));
        assert_eq!(pm.node_type(&test_overlay(3)), Some(SwarmNodeType::Storer));
    }
}
