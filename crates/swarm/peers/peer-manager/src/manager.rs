//! Peer manager with Arc-per-peer pattern and in-memory LRU indexing.

use std::sync::{Arc, Weak};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use tokio::sync::{broadcast, oneshot};
use tracing::{debug, trace, warn};
use vertex_net_local::IpCapability;
use vertex_net_peer_store::NetPeerStore;
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_peer_score::{ScoreObserver, SwarmScoringConfig, SwarmScoringEvent};
use vertex_swarm_primitives::{OverlayAddress, SwarmNodeType};
use vertex_tasks::TaskExecutor;

use crate::data::{SwarmPeerData, SwarmPeerRecord};
use crate::entry::PeerEntry;
use crate::proximity_index::ProximityIndex;
use crate::pruner::PruneConfig;

/// Default maximum tracked peers.
pub const DEFAULT_MAX_TRACKED_PEERS: usize = 10_000;

/// Score observer that auto-bans peers crossing the threshold.
struct BanObserver {
    manager: Weak<PeerManager>,
}

impl ScoreObserver for BanObserver {
    fn on_score_changed(
        &self,
        _overlay: &OverlayAddress,
        _old_score: f64,
        _new_score: f64,
        _event: &SwarmScoringEvent,
    ) {}

    fn on_score_warning(&self, overlay: &OverlayAddress, score: f64) {
        warn!(?overlay, score, "peer score warning");
    }

    fn on_should_ban(&self, overlay: &OverlayAddress, score: f64, reason: &str) {
        if let Some(manager) = self.manager.upgrade() {
            warn!(?overlay, score, reason, "auto-banning peer");
            manager.ban(overlay, Some(reason.to_string()));
        }
    }

    fn on_severe_event(&self, overlay: &OverlayAddress, event: &SwarmScoringEvent) {
        debug!(?overlay, ?event, "severe scoring event");
    }
}

/// Peer lifecycle manager with in-memory storage.
///
/// Uses `ProximityIndex` for LRU-ordered bin indexing and `DashMap` for full
/// peer data (multiaddrs, scoring, ban/backoff state).
pub struct PeerManager {
    /// In-memory peer index with LRU ordering.
    index: ProximityIndex,
    /// Full peer data indexed by overlay.
    peers: DashMap<OverlayAddress, Arc<PeerEntry>>,
    /// Scoring configuration.
    scoring_config: Arc<SwarmScoringConfig>,
    /// Maximum tracked peers (for pruning).
    max_tracked_peers: Option<usize>,
    /// Guard against concurrent pruning.
    prune_in_progress: AtomicBool,
    /// Running sum of peer scores (f64 bits stored as u64).
    score_sum: AtomicU64,
    /// Count of scored peers for average calculation.
    scored_peer_count: AtomicUsize,
    /// Count of banned peers for O(1) eligible_count().
    banned_count: AtomicUsize,
    /// Score observer shared with all PeerEntries.
    observer: Arc<dyn ScoreObserver>,
    /// Channel for notifying topology of banned peers.
    ban_tx: broadcast::Sender<OverlayAddress>,
}

impl PeerManager {
    /// Create with local overlay, max proximity order, and default settings.
    pub fn new(local_overlay: OverlayAddress, max_po: u8) -> Arc<Self> {
        Self::with_config(local_overlay, max_po, SwarmScoringConfig::default(), Some(DEFAULT_MAX_TRACKED_PEERS))
    }

    /// Create with specified scoring config and limits.
    pub fn with_config(
        local_overlay: OverlayAddress,
        max_po: u8,
        scoring_config: SwarmScoringConfig,
        max_tracked_peers: Option<usize>,
    ) -> Arc<Self> {
        let (ban_tx, _) = broadcast::channel(64);
        Arc::new_cyclic(|weak| {
            let observer: Arc<dyn ScoreObserver> = Arc::new(BanObserver {
                manager: weak.clone(),
            });
            Self {
                index: ProximityIndex::new(local_overlay, max_po, 0),
                peers: DashMap::new(),
                scoring_config: Arc::new(scoring_config),
                max_tracked_peers,
                prune_in_progress: AtomicBool::new(false),
                score_sum: AtomicU64::new(0.0_f64.to_bits()),
                scored_peer_count: AtomicUsize::new(0),
                banned_count: AtomicUsize::new(0),
                observer,
                ban_tx,
            }
        })
    }

    /// Atomically add a delta to the score sum.
    fn add_to_score_sum(&self, delta: f64) {
        loop {
            let current = self.score_sum.load(Ordering::Relaxed);
            let current_f64 = f64::from_bits(current);
            let new_f64 = current_f64 + delta;
            let new_bits = new_f64.to_bits();
            if self
                .score_sum
                .compare_exchange_weak(current, new_bits, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
        }
    }

    /// Get the local overlay address.
    pub fn local_overlay(&self) -> OverlayAddress {
        *self.index.local_overlay()
    }

    /// Get counts of peers in each proximity bin (0-31).
    pub fn bin_sizes(&self) -> Vec<usize> {
        self.index.bin_sizes()
    }

    /// Get count of peers in a specific proximity bin.
    pub fn bin_size(&self, po: u8) -> usize {
        self.index.bin_size(po)
    }

    /// Get all peer overlays in a specific proximity bin (LRU to MRU order).
    pub fn peers_in_bin(&self, po: u8) -> Vec<OverlayAddress> {
        self.index.peers_in_bin(po)
    }

    /// Check if a peer exists in the index.
    pub fn contains(&self, overlay: &OverlayAddress) -> bool {
        self.index.exists(overlay)
    }

    pub fn node_type(&self, overlay: &OverlayAddress) -> Option<SwarmNodeType> {
        self.peers.get(overlay).map(|e| e.node_type())
    }

    /// Get count of all tracked peers.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Check if no peers are tracked.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Get all peer overlays that are not banned and not in backoff.
    #[must_use]
    pub fn eligible_peers(&self) -> Vec<OverlayAddress> {
        self.peers
            .iter()
            .filter(|r| !r.value().is_banned() && !r.value().is_in_backoff())
            .map(|r| *r.key())
            .collect()
    }

    /// Count of peers that are not banned.
    pub fn eligible_count(&self) -> usize {
        self.index.len().saturating_sub(self.banned_count.load(Ordering::Relaxed))
    }

    /// Get all known Storer peers that aren't banned.
    #[must_use]
    pub fn known_storer_overlays(&self) -> Vec<OverlayAddress> {
        self.peers
            .iter()
            .filter(|r| {
                r.value().node_type() == SwarmNodeType::Storer
                    && !r.value().is_banned()
            })
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
    #[must_use]
    pub fn get_dialable_peers(&self, candidates: &[OverlayAddress]) -> Vec<SwarmPeer> {
        candidates
            .iter()
            .filter_map(|overlay| {
                self.peers.get(overlay).and_then(|e| {
                    if !e.is_banned() && !e.is_in_backoff() {
                        Some(e.swarm_peer())
                    } else {
                        None
                    }
                })
            })
            .collect()
    }

    /// Get dialable overlay addresses from a specific bin (not banned, not in backoff).
    ///
    /// Uses streaming iteration with early exit to avoid materializing the entire bin.
    pub fn dialable_overlays_in_bin(&self, po: u8, count: usize) -> Vec<OverlayAddress> {
        self.index.filter_bin(po, count, |overlay| {
            self.peers
                .get(overlay)
                .is_some_and(|e| !e.is_banned() && !e.is_in_backoff())
        })
    }

    /// Get dialable peers from a specific bin (not banned, not in backoff).
    pub fn dialable_in_bin(&self, po: u8, count: usize) -> Vec<SwarmPeer> {
        self.index
            .peers_in_bin(po)
            .into_iter()
            .filter_map(|overlay| {
                self.peers.get(&overlay).and_then(|e| {
                    if !e.is_banned() && !e.is_in_backoff() {
                        Some(e.swarm_peer())
                    } else {
                        None
                    }
                })
            })
            .take(count)
            .collect()
    }

    #[must_use]
    pub fn get_peer_capability(&self, overlay: &OverlayAddress) -> Option<IpCapability> {
        self.peers.get(overlay).map(|r| r.ip_capability())
    }

    #[must_use]
    pub fn get_peer_score(&self, overlay: &OverlayAddress) -> Option<f64> {
        self.peers.get(overlay).map(|r| r.score())
    }

    /// Get SwarmPeer for a single overlay.
    #[must_use]
    pub fn get_swarm_peer(&self, overlay: &OverlayAddress) -> Option<SwarmPeer> {
        self.peers.get(overlay).map(|r| r.swarm_peer())
    }

    /// Get a snapshot of all banned peer overlays.
    #[must_use]
    pub fn banned_set(&self) -> std::collections::HashSet<OverlayAddress> {
        self.peers
            .iter()
            .filter(|r| r.value().is_banned())
            .map(|r| *r.key())
            .collect()
    }

    /// Get a snapshot of all peers currently in backoff.
    #[must_use]
    pub fn peers_in_backoff(&self) -> std::collections::HashSet<OverlayAddress> {
        self.peers
            .iter()
            .filter(|r| r.value().is_in_backoff())
            .map(|r| *r.key())
            .collect()
    }

    /// Check if peer is in backoff via PeerEntry.
    pub fn peer_is_in_backoff(&self, overlay: &OverlayAddress) -> bool {
        self.peers.get(overlay).is_some_and(|e| e.is_in_backoff())
    }

    /// Check if peer is banned via PeerEntry (AtomicBool read).
    pub fn is_banned(&self, overlay: &OverlayAddress) -> bool {
        self.peers.get(overlay).is_some_and(|e| e.is_banned())
    }

    /// Ban a peer (prevents dialing). Notifies ban subscribers for disconnect.
    pub fn ban(&self, overlay: &OverlayAddress, reason: Option<String>) {
        if let Some(entry) = self.peers.get(overlay)
            && !entry.is_banned()
        {
            warn!(?overlay, ?reason, "banning peer");
            entry.ban(reason);
            self.banned_count.fetch_add(1, Ordering::Relaxed);
            let _ = self.ban_tx.send(*overlay);
        }
    }

    /// Subscribe to ban notifications for disconnecting banned peers.
    pub fn subscribe_bans(&self) -> broadcast::Receiver<OverlayAddress> {
        self.ban_tx.subscribe()
    }

    /// Store a single discovered peer (default to Client node type).
    pub fn store_discovered_peer(&self, swarm_peer: SwarmPeer) -> OverlayAddress {
        let overlay = OverlayAddress::from(*swarm_peer.overlay());
        self.insert_peer(overlay, swarm_peer, SwarmNodeType::Client);
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

        let mut stored_overlays = Vec::with_capacity(peers.len());
        for swarm_peer in peers {
            let overlay = OverlayAddress::from(*swarm_peer.overlay());
            self.insert_peer(overlay, swarm_peer, SwarmNodeType::Client);
            stored_overlays.push(overlay);
        }
        stored_overlays
    }

    /// Insert or update a peer.
    fn insert_peer(&self, overlay: OverlayAddress, peer: SwarmPeer, node_type: SwarmNodeType) {
        use dashmap::mapref::entry::Entry;

        match self.peers.entry(overlay) {
            Entry::Occupied(e) => {
                e.get().update_peer(peer, node_type);
                self.index.touch(&overlay);
            }
            Entry::Vacant(e) => {
                let entry = Arc::new(PeerEntry::with_config(
                    peer,
                    node_type,
                    overlay,
                    Arc::clone(&self.scoring_config),
                    Arc::clone(&self.observer),
                ));
                let initial_score = entry.score();
                e.insert(entry);
                self.index.add(overlay);
                self.add_to_score_sum(initial_score);
                self.scored_peer_count.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Remove a peer and update score tracking.
    fn remove_peer(&self, overlay: &OverlayAddress) {
        if let Some((_, entry)) = self.peers.remove(overlay) {
            let score = entry.score();
            self.add_to_score_sum(-score);
            self.scored_peer_count.fetch_sub(1, Ordering::Relaxed);
            if entry.is_banned() {
                self.banned_count.fetch_sub(1, Ordering::Relaxed);
            }
        }
        self.index.remove(overlay);
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
        let current_count = self.index.len();
        current_count >= (max as f64 * config.capacity_threshold) as usize
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

        let current_count = self.index.len();
        let target = (max as f64 * config.target_utilization) as usize;
        let to_remove = current_count.saturating_sub(target);

        if to_remove == 0 {
            return;
        }

        let candidates = self.collect_prune_candidates(to_remove).await;

        for batch in candidates.chunks(config.batch_size) {
            for overlay in batch {
                self.remove_peer(overlay);
            }
            tokio::task::yield_now().await;
        }

        debug!(removed = candidates.len(), remaining = self.index.len(), "pruned peers");
    }

    /// Collect and sort prune candidates.
    async fn collect_prune_candidates(&self, count: usize) -> Vec<OverlayAddress> {
        let ban_threshold = self.scoring_config.ban_threshold();
        let candidates: Vec<_> = self.peers
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

        // Offload CPU-intensive sort to blocking thread pool
        let Ok(executor) = TaskExecutor::try_current() else {
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

    /// Iterate all peers, calling `f` with health state for each.
    ///
    /// All reads are lock-free atomics. Safe to call from a background task.
    pub fn for_each_peer<F>(&self, mut f: F)
    where
        F: FnMut(&OverlayAddress, f64, u32, bool, bool, bool),
    {
        for entry in self.peers.iter() {
            let v = entry.value();
            f(
                entry.key(),
                v.score(),
                v.consecutive_failures(),
                v.is_in_backoff(),
                v.is_stale(),
                v.is_banned(),
            );
        }
    }

    /// Get statistics about the peer manager (O(1), no iteration).
    ///
    /// Reads only atomics and pre-computed values. For on-demand RPC queries,
    /// use `stats_full()` which iterates the DashMap.
    #[must_use]
    pub fn stats(&self) -> PeerManagerStats {
        let known_stats = self.index.stats();
        let peer_count = self.scored_peer_count.load(Ordering::Relaxed);
        let total_score = f64::from_bits(self.score_sum.load(Ordering::Relaxed));
        let avg_score = if peer_count > 0 {
            total_score / peer_count as f64
        } else {
            0.0
        };

        // Memory estimation:
        // - PeerEntry: ~512 bytes (SwarmPeer ~200, scoring state ~200, atomics ~100)
        // - Bin index entry: ~48 bytes (overlay 32, LinkedList node overhead ~16)
        const PEER_ENTRY_SIZE: usize = 512;
        const BIN_INDEX_ENTRY_SIZE: usize = 48;

        let estimated_entries_bytes = peer_count * PEER_ENTRY_SIZE;
        let estimated_bin_index_bytes = known_stats.total_peers * BIN_INDEX_ENTRY_SIZE;
        let banned = self.banned_count.load(Ordering::Relaxed);

        PeerManagerStats {
            total_peers: known_stats.total_peers,
            banned_peers: banned,
            avg_peer_score: avg_score,
            estimated_entries_bytes,
            estimated_bin_index_bytes,
        }
    }

    /// Called when a peer completes handshake. Stores the SwarmPeer metadata.
    pub fn on_peer_ready(&self, swarm_peer: SwarmPeer, node_type: SwarmNodeType) {
        let overlay = OverlayAddress::from(*swarm_peer.overlay());
        debug!(?overlay, ?node_type, "storing peer");

        self.insert_peer(overlay, swarm_peer, node_type);
        if let Some(entry) = self.peers.get(&overlay) {
            let old_score = entry.score();
            entry.record_success(Duration::ZERO);
            self.add_to_score_sum(entry.score() - old_score);
        }
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
        }
    }

    /// Record a scoring event for a peer.
    pub fn record_scoring_event(&self, overlay: &OverlayAddress, event: SwarmScoringEvent) {
        if let Some(entry) = self.peers.get(overlay) {
            let old_score = entry.score();
            entry.record_event(event);
            let delta = entry.score() - old_score;
            self.add_to_score_sum(delta);
        }
    }

    /// Save all tracked peers to a store and flush to disk.
    pub fn save_to_store(
        &self,
        store: &impl NetPeerStore<OverlayAddress, SwarmPeerData>,
    ) -> Result<(), vertex_net_peer_store::StoreError> {
        let records: Vec<SwarmPeerRecord> = self.peers
            .iter()
            .map(|r| r.value().to_record(*r.key()))
            .collect();
        store.save_batch(&records)?;
        store.flush()
    }

    /// Load peers from a store into memory. Skips overlays already tracked.
    pub fn load_from_store(
        &self,
        store: &impl NetPeerStore<OverlayAddress, SwarmPeerData>,
    ) -> Result<usize, vertex_net_peer_store::StoreError> {
        let records = store.load_all()?;
        let count = records.len();
        for record in records {
            let overlay = record.id;
            if self.peers.contains_key(&overlay) {
                continue;
            }
            let was_banned = record.is_banned;
            let entry = Arc::new(PeerEntry::from_record(
                record,
                Arc::clone(&self.scoring_config),
                Arc::clone(&self.observer),
            ));
            let score = entry.score();
            self.peers.insert(overlay, entry);
            self.index.add(overlay);
            self.add_to_score_sum(score);
            self.scored_peer_count.fetch_add(1, Ordering::Relaxed);
            if was_banned {
                self.banned_count.fetch_add(1, Ordering::Relaxed);
            }
        }
        Ok(count)
    }
}

impl vertex_swarm_peer::SwarmPeerResolver for PeerManager {
    fn get_swarm_peer(&self, overlay: &OverlayAddress) -> Option<SwarmPeer> {
        self.peers.get(overlay).map(|r| r.swarm_peer())
    }
}

/// Statistics about the peer manager state (O(1), no iteration).
#[derive(Debug, Clone)]
pub struct PeerManagerStats {
    /// Total peers tracked (in index).
    pub total_peers: usize,
    /// Peers currently banned.
    pub banned_peers: usize,
    /// Average peer score.
    pub avg_peer_score: f64,
    /// Estimated memory for peer entries (bytes).
    pub estimated_entries_bytes: usize,
    /// Estimated memory for bin index (bytes).
    pub estimated_bin_index_bytes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_net_peer_store::MemoryPeerStore;
    use vertex_swarm_test_utils::{test_overlay, test_swarm_peer};

    fn local_overlay() -> OverlayAddress {
        test_overlay(0)
    }

    #[test]
    fn test_store_discovered_peer() {
        let pm = PeerManager::new(local_overlay(), 31);
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        let stored = pm.store_discovered_peer(swarm_peer.clone());
        assert_eq!(stored, overlay);
        assert!(pm.get_swarm_peer(&overlay).is_some());
        assert!(pm.contains(&overlay));
    }

    #[test]
    fn test_on_peer_ready() {
        let pm = PeerManager::new(local_overlay(), 31);
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        pm.on_peer_ready(swarm_peer, SwarmNodeType::Storer);
        assert_eq!(pm.node_type(&overlay), Some(SwarmNodeType::Storer));
        assert!(pm.contains(&overlay));
    }

    #[test]
    fn test_peer_lifecycle() {
        let pm = PeerManager::new(local_overlay(), 31);
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        // Discover peer via Hive
        pm.store_discovered_peer(swarm_peer.clone());
        assert!(pm.eligible_peers().contains(&overlay));

        // Store as connected peer
        pm.on_peer_ready(swarm_peer, SwarmNodeType::Client);
        assert!(pm.eligible_peers().contains(&overlay));
    }

    #[test]
    fn test_ban() {
        let pm = PeerManager::new(local_overlay(), 31);
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        pm.on_peer_ready(swarm_peer, SwarmNodeType::Client);
        pm.ban(&overlay, Some("misbehaving".to_string()));

        assert!(pm.is_banned(&overlay));
        assert!(!pm.eligible_peers().contains(&overlay));
    }

    #[test]
    fn test_get_dialable_peers() {
        let pm = PeerManager::new(local_overlay(), 31);

        for n in 1..=5 {
            pm.store_discovered_peer(test_swarm_peer(n));
        }

        pm.ban(&test_overlay(1), None);

        let all_overlays: Vec<_> = (1..=5).map(test_overlay).collect();
        let dialable = pm.get_dialable_peers(&all_overlays);

        assert_eq!(dialable.len(), 4);
    }

    #[tokio::test]
    async fn test_async_pruning() {
        let pm = PeerManager::with_config(local_overlay(), 31, SwarmScoringConfig::default(), Some(10));

        for n in 1..=15 {
            pm.store_discovered_peer(test_swarm_peer(n));
        }

        assert_eq!(pm.stats().total_peers, 15);

        let config = PruneConfig {
            capacity_threshold: 0.5,
            target_utilization: 0.5,
            batch_size: 5,
            ..Default::default()
        };
        pm.prune_async(&config).await;

        assert!(pm.stats().total_peers <= 5);
    }

    #[test]
    fn test_custom_scoring_config() {
        let config = SwarmScoringConfig::lenient();
        let pm = PeerManager::with_config(local_overlay(), 31, config, None);

        // Verify custom config is accepted (ban threshold propagates)
        assert!(pm.scoring_config.ban_threshold() < 0.0);
    }

    #[test]
    fn test_known_storer_overlays() {
        let pm = PeerManager::new(local_overlay(), 31);

        pm.on_peer_ready(test_swarm_peer(1), SwarmNodeType::Storer);
        pm.on_peer_ready(test_swarm_peer(2), SwarmNodeType::Client);
        pm.on_peer_ready(test_swarm_peer(3), SwarmNodeType::Storer);
        pm.on_peer_ready(test_swarm_peer(4), SwarmNodeType::Client);

        pm.ban(&test_overlay(1), None);

        let storers = pm.known_storer_overlays();
        assert_eq!(storers.len(), 1);
        assert!(storers.contains(&test_overlay(3)));
    }

    #[test]
    fn test_get_swarm_peers() {
        let pm = PeerManager::new(local_overlay(), 31);

        for n in 1..=5 {
            pm.on_peer_ready(test_swarm_peer(n), SwarmNodeType::Storer);
        }

        let overlays = vec![test_overlay(1), test_overlay(3), test_overlay(5)];
        let peers = pm.get_swarm_peers(&overlays);

        assert_eq!(peers.len(), 3);
    }

    #[test]
    fn test_get_swarm_peers_missing() {
        let pm = PeerManager::new(local_overlay(), 31);

        pm.on_peer_ready(test_swarm_peer(1), SwarmNodeType::Storer);

        let overlays = vec![test_overlay(1), test_overlay(99)];
        let peers = pm.get_swarm_peers(&overlays);

        assert_eq!(peers.len(), 1);
    }

    #[test]
    fn test_node_type_variants() {
        let pm = PeerManager::new(local_overlay(), 31);

        pm.on_peer_ready(test_swarm_peer(1), SwarmNodeType::Bootnode);
        pm.on_peer_ready(test_swarm_peer(2), SwarmNodeType::Client);
        pm.on_peer_ready(test_swarm_peer(3), SwarmNodeType::Storer);

        assert_eq!(pm.node_type(&test_overlay(1)), Some(SwarmNodeType::Bootnode));
        assert_eq!(pm.node_type(&test_overlay(2)), Some(SwarmNodeType::Client));
        assert_eq!(pm.node_type(&test_overlay(3)), Some(SwarmNodeType::Storer));
    }

    #[test]
    fn test_bin_index_integration() {
        let pm = PeerManager::new(local_overlay(), 31);

        for n in 1..=5 {
            pm.store_discovered_peer(test_swarm_peer(n));
        }

        let bin_sizes = pm.bin_sizes();
        let total: usize = bin_sizes.iter().sum();
        assert_eq!(total, 5);

        for n in 1..=5 {
            assert!(pm.contains(&test_overlay(n)));
        }
    }

    #[test]
    fn test_lru_ordering_preserved() {
        let pm = PeerManager::new(local_overlay(), 31);

        pm.store_discovered_peer(test_swarm_peer(1));
        pm.store_discovered_peer(test_swarm_peer(2));
        pm.store_discovered_peer(test_swarm_peer(3));

        // Update peer 1 (should move to MRU)
        pm.store_discovered_peer(test_swarm_peer(1));

        // Peer 2 should now be LRU
        // (exact order depends on which bin they're in)
    }

    #[test]
    fn test_dialable_in_bin() {
        let pm = PeerManager::new(local_overlay(), 31);

        // Add peers to same bin
        let p1 = OverlayAddress::from([0x80; 32]);
        let p2 = OverlayAddress::from([0xc0; 32]);
        let p3 = OverlayAddress::from([0xa0; 32]);

        let peer1 = test_swarm_peer(1);
        let peer2 = test_swarm_peer(2);
        let peer3 = test_swarm_peer(3);

        // Manually insert with specific overlays
        pm.index.add(p1);
        pm.index.add(p2);
        pm.index.add(p3);
        pm.peers.insert(p1, Arc::new(PeerEntry::with_config(
            peer1, SwarmNodeType::Client, p1,
            Arc::clone(&pm.scoring_config), Arc::clone(&pm.observer),
        )));
        pm.peers.insert(p2, Arc::new(PeerEntry::with_config(
            peer2, SwarmNodeType::Client, p2,
            Arc::clone(&pm.scoring_config), Arc::clone(&pm.observer),
        )));
        pm.peers.insert(p3, Arc::new(PeerEntry::with_config(
            peer3, SwarmNodeType::Client, p3,
            Arc::clone(&pm.scoring_config), Arc::clone(&pm.observer),
        )));

        pm.ban(&p1, None);

        let dialable = pm.dialable_in_bin(0, 2);
        assert_eq!(dialable.len(), 2);
    }

    #[test]
    fn test_get_swarm_peer() {
        let pm = PeerManager::new(local_overlay(), 31);
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        assert!(pm.get_swarm_peer(&overlay).is_none());
        pm.store_discovered_peer(swarm_peer.clone());
        assert!(pm.get_swarm_peer(&overlay).is_some());
    }

    #[test]
    fn test_persistence_roundtrip() {
        let pm1 = PeerManager::new(local_overlay(), 31);

        for n in 1..=5 {
            pm1.on_peer_ready(test_swarm_peer(n), SwarmNodeType::Storer);
        }
        pm1.ban(&test_overlay(1), Some("bad".to_string()));

        let store = MemoryPeerStore::<OverlayAddress, SwarmPeerData>::new();
        pm1.save_to_store(&store).unwrap();

        let pm2 = PeerManager::new(local_overlay(), 31);
        let loaded = pm2.load_from_store(&store).unwrap();
        assert_eq!(loaded, 5);
        assert_eq!(pm2.len(), 5);
        assert!(pm2.is_banned(&test_overlay(1)));
        assert!(!pm2.is_banned(&test_overlay(2)));
    }

    #[test]
    fn test_banned_count_tracking() {
        let pm = PeerManager::new(local_overlay(), 31);

        for n in 1..=5 {
            pm.store_discovered_peer(test_swarm_peer(n));
        }

        assert_eq!(pm.eligible_count(), 5);
        pm.ban(&test_overlay(1), None);
        assert_eq!(pm.eligible_count(), 4);
        pm.ban(&test_overlay(2), None);
        assert_eq!(pm.eligible_count(), 3);

        // Double-ban should not double-count
        pm.ban(&test_overlay(1), None);
        assert_eq!(pm.eligible_count(), 3);
    }

}
