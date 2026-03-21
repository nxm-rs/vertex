//! Peer manager with hot/cold architecture and database-backed persistence.
//!
//! Connected and recently-accessed peers live in a hot DashMap cache.
//! All known overlays are tracked in the ProximityIndex. Peer data for
//! cold peers lives in the database and is loaded on demand.

use std::marker::PhantomData;
use std::sync::{Arc, Weak};
use std::time::Duration;

use dashmap::{DashMap, DashSet};
use metrics::gauge;
use tokio::sync::broadcast;
use tracing::{debug, warn};
use vertex_net_local::IpCapability;
use vertex_net_peer_store::NetPeerStore;
use vertex_net_peer_store::error::StoreError;
use vertex_swarm_api::{SwarmIdentity, SwarmPeerResolver, SwarmScoreStore, SwarmSpec};
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_peer_score::{PeerScore, ScoreCallbacks, SwarmScoringConfig};
use vertex_swarm_primitives::{OverlayAddress, SwarmNodeType};

use crate::entry::{
    HealthState, PeerEntry, StoredPeer, on_health_added, on_health_changed, on_health_removed,
    unix_timestamp_secs,
};
use crate::proximity_index::{AddError, ProximityIndex};
use crate::score_distribution::ScoreDistribution;
use crate::write_buffer::WriteBuffer;

/// Default maximum peers per proximity bin in the index.
///
/// With topology targets of 3-35 peers per bin, 128 gives 3.7-42x headroom.
pub(crate) const DEFAULT_MAX_PER_BIN: usize = 128;

/// Default maximum hot peers in DashMap cache.
const DEFAULT_MAX_HOT_PEERS: usize = 500;

/// Default write buffer capacity before auto-flush.
const DEFAULT_WRITE_BUFFER_CAPACITY: usize = 64;

/// Peer lifecycle manager with hot/cold storage architecture.
///
/// All known overlays are tracked in the `ProximityIndex` (~80 bytes each).
/// Full peer data lives in either:
/// - **Hot cache** (`DashMap`): connected + recently-accessed peers (~200-500)
/// - **Cold storage** (DB via `NetPeerStore`): all peers, loaded on demand
///
/// When no store is configured (tests/ephemeral), all peers live in DashMap.
pub struct PeerManager<I: SwarmIdentity> {
    pub(crate) _identity: PhantomData<I>,
    /// In-memory peer index with LRU ordering (ALL known overlays).
    pub(crate) index: ProximityIndex,
    /// Hot cache: connected + recently-accessed peers.
    pub(crate) peers: DashMap<OverlayAddress, Arc<PeerEntry>>,
    /// Database backend for cold storage (None for ephemeral/test mode).
    pub(crate) store: Option<Arc<dyn NetPeerStore<StoredPeer>>>,
    /// Score persistence (None for ephemeral/test mode).
    pub(crate) score_store: Option<Arc<dyn SwarmScoreStore<Value = PeerScore, Error = StoreError>>>,
    /// O(1) ban checks without DB or DashMap lookup.
    pub(crate) banned_set: DashSet<OverlayAddress>,
    /// Batches DB writes for amortized flush.
    pub(crate) write_buffer: WriteBuffer,
    /// Scoring configuration.
    pub(crate) scoring_config: Arc<SwarmScoringConfig>,
    /// Maximum peers in hot DashMap cache before eviction.
    pub(crate) max_hot_peers: usize,
    /// Callbacks shared with all PeerEntries.
    pub(crate) callbacks: Arc<ScoreCallbacks>,
    /// Per-bucket gauge tracking of score distribution.
    pub(crate) score_distribution: Arc<ScoreDistribution>,
    /// Channel for notifying topology of banned peers.
    pub(crate) ban_tx: broadcast::Sender<OverlayAddress>,
}

impl<I: SwarmIdentity> PeerManager<I> {
    /// Create with identity and default settings (no persistent store).
    pub fn new(identity: &I) -> Arc<Self> {
        Self::with_config(identity, SwarmScoringConfig::default(), DEFAULT_MAX_PER_BIN)
    }

    /// Create with specified scoring config and per-bin limit (no persistent store).
    pub fn with_config(
        identity: &I,
        scoring_config: SwarmScoringConfig,
        max_per_bin: usize,
    ) -> Arc<Self> {
        Self::build(
            identity,
            scoring_config,
            max_per_bin,
            None,
            None,
            DEFAULT_MAX_HOT_PEERS,
        )
    }

    /// Create with a database-backed persistent store.
    ///
    /// Loads the overlay index and banned set from the store on construction.
    /// Hot cache starts empty; peers are promoted on access.
    /// Scores are loaded lazily when peers are promoted to the hot cache.
    pub fn with_store(
        identity: &I,
        store: Arc<dyn NetPeerStore<StoredPeer>>,
        score_store: Option<Arc<dyn SwarmScoreStore<Value = PeerScore, Error = StoreError>>>,
        scoring_config: SwarmScoringConfig,
        max_per_bin: usize,
    ) -> Arc<Self> {
        let pm = Self::build(
            identity,
            scoring_config,
            max_per_bin,
            Some(store),
            score_store,
            DEFAULT_MAX_HOT_PEERS,
        );
        pm.load_index_from_store();
        pm
    }

    pub(crate) fn build(
        identity: &I,
        scoring_config: SwarmScoringConfig,
        max_per_bin: usize,
        store: Option<Arc<dyn NetPeerStore<StoredPeer>>>,
        score_store: Option<Arc<dyn SwarmScoreStore<Value = PeerScore, Error = StoreError>>>,
        max_hot_peers: usize,
    ) -> Arc<Self> {
        let local_overlay = identity.overlay_address();
        let max_po = identity.spec().max_po();
        let (ban_tx, _) = broadcast::channel(64);
        let score_distribution = Arc::new(ScoreDistribution::new());
        Arc::new_cyclic(|weak: &Weak<Self>| {
            let callbacks = Arc::new(ScoreCallbacks {
                on_score_changed: {
                    let sd = Arc::clone(&score_distribution);
                    Box::new(move |_overlay, old, new, _event| {
                        sd.on_score_changed(old, new);
                    })
                },
                on_score_warning: Box::new(|overlay, score| {
                    warn!(?overlay, score, "peer score warning");
                }),
                on_should_ban: {
                    let w = weak.clone();
                    Box::new(move |overlay, score, reason| {
                        if let Some(manager) = w.upgrade() {
                            warn!(?overlay, score, reason, "auto-banning peer");
                            manager.ban(overlay, Some(reason.to_string()));
                        }
                    })
                },
                on_severe_event: Box::new(|overlay, event| {
                    debug!(?overlay, ?event, "severe scoring event");
                }),
            });
            Self {
                _identity: PhantomData,
                index: ProximityIndex::new(local_overlay, max_po, max_per_bin),
                peers: DashMap::new(),
                store,
                score_store,
                banned_set: DashSet::new(),
                write_buffer: WriteBuffer::new(DEFAULT_WRITE_BUFFER_CAPACITY),
                scoring_config: Arc::new(scoring_config),
                max_hot_peers,
                callbacks,
                score_distribution,
                ban_tx,
            }
        })
    }

    /// Get the score distribution tracker for emitting gauge metrics.
    pub fn score_distribution(&self) -> &Arc<ScoreDistribution> {
        &self.score_distribution
    }

    /// Direct access to the proximity index for read-only queries.
    pub fn index(&self) -> &ProximityIndex {
        &self.index
    }

    #[must_use]
    pub fn node_type(&self, overlay: &OverlayAddress) -> Option<SwarmNodeType> {
        if let Some(entry) = self.peers.get(overlay) {
            return Some(entry.node_type());
        }
        if let Some(ref store) = self.store
            && let Ok(Some(record)) = store.get(overlay)
        {
            return Some(record.node_type);
        }
        None
    }

    /// Get all peer overlays that are not banned and not in backoff.
    ///
    /// Without a store, iterates DashMap (exact). With a store, returns all
    /// known overlays minus banned (may include peers in backoff).
    #[must_use]
    pub fn eligible_peers(&self) -> Vec<OverlayAddress> {
        if self.store.is_none() {
            return self
                .peers
                .iter()
                .filter(|r| r.value().is_dialable())
                .map(|r| *r.key())
                .collect();
        }
        self.index
            .all_peers()
            .into_iter()
            .filter(|overlay| !self.banned_set.contains(overlay))
            .collect()
    }

    /// Count of peers that are not banned (O(1)).
    #[must_use]
    pub fn eligible_count(&self) -> usize {
        self.index.len().saturating_sub(self.banned_set.len())
    }

    /// Get all known Storer peers that aren't banned.
    ///
    /// Checks hot cache first, then falls back to cold store for peers in the
    /// index that aren't in the hot cache.
    #[must_use]
    pub fn known_storer_overlays(&self) -> Vec<OverlayAddress> {
        let mut storers = Vec::new();

        for (_, overlay) in self.index.iter_by_proximity() {
            if self.banned_set.contains(&overlay) {
                continue;
            }
            if let Some(entry) = self.peers.get(&overlay) {
                if entry.node_type() == SwarmNodeType::Storer {
                    storers.push(overlay);
                }
                continue;
            }
            // Cold peer - check store
            if let Some(ref store) = self.store
                && let Ok(Some(record)) = store.get(&overlay)
                && record.node_type == SwarmNodeType::Storer
                && record.ban_info.is_none()
            {
                storers.push(overlay);
            }
        }

        storers
    }

    /// Get known storer overlays in a specific proximity bin (not banned).
    #[must_use]
    pub fn storer_overlays_in_bin(&self, po: u8, count: usize) -> Vec<OverlayAddress> {
        let mut result = Vec::new();
        let mut cold_candidates = Vec::new();

        self.index.filter_bin(po, count + count, |overlay| {
            if self.banned_set.contains(overlay) {
                return false;
            }
            if let Some(e) = self.peers.get(overlay) {
                if e.node_type() == SwarmNodeType::Storer {
                    result.push(*overlay);
                }
                return false;
            }
            cold_candidates.push(*overlay);
            false
        });

        if result.len() < count
            && let Some(ref store) = self.store
        {
            for overlay in &cold_candidates {
                if result.len() >= count {
                    break;
                }
                if let Ok(Some(record)) = store.get(overlay)
                    && record.node_type == SwarmNodeType::Storer
                    && !record.is_banned()
                {
                    result.push(*overlay);
                }
            }
        }

        result.truncate(count);
        result
    }

    /// Get SwarmPeer data for multiple overlays (promotes cold peers to hot).
    #[must_use]
    pub fn get_swarm_peers(&self, overlays: &[OverlayAddress]) -> Vec<SwarmPeer> {
        overlays
            .iter()
            .filter_map(|o| self.get_or_load(o).map(|e| e.swarm_peer()))
            .collect()
    }

    /// Get SwarmPeers for candidates that are not banned and not in backoff.
    #[must_use]
    pub fn get_dialable_peers(&self, candidates: &[OverlayAddress]) -> Vec<SwarmPeer> {
        candidates
            .iter()
            .filter_map(|overlay| {
                if self.banned_set.contains(overlay) {
                    return None;
                }
                let entry = self.get_or_load(overlay)?;
                entry.is_dialable().then(|| entry.swarm_peer())
            })
            .collect()
    }

    /// Get dialable overlay addresses from a specific bin (not banned, not in backoff).
    pub fn dialable_overlays_in_bin(&self, po: u8, count: usize) -> Vec<OverlayAddress> {
        let mut result = Vec::new();
        let mut cold_candidates = Vec::new();

        self.index.filter_bin(po, count + count, |overlay| {
            if self.banned_set.contains(overlay) {
                return false;
            }
            if let Some(entry) = self.peers.get(overlay) {
                if entry.is_dialable() {
                    result.push(*overlay);
                }
                return false;
            }
            cold_candidates.push(*overlay);
            false
        });

        if result.len() < count {
            for overlay in &cold_candidates {
                if result.len() >= count {
                    break;
                }
                if self.is_cold_peer_dialable(overlay) {
                    result.push(*overlay);
                }
            }
        }

        result.truncate(count);
        result
    }

    /// Get dialable peers from a specific bin (not banned, not in backoff).
    pub fn dialable_in_bin(&self, po: u8, count: usize) -> Vec<SwarmPeer> {
        let overlays = self.dialable_overlays_in_bin(po, count);
        self.get_swarm_peers(&overlays)
    }

    #[must_use]
    pub fn get_peer_capability(&self, overlay: &OverlayAddress) -> Option<IpCapability> {
        self.peers.get(overlay).map(|r| r.ip_capability())
    }

    #[must_use]
    pub fn get_peer_score(&self, overlay: &OverlayAddress) -> Option<f64> {
        self.peers.get(overlay).map(|r| r.score())
    }

    /// Get SwarmPeer for a single overlay (promotes cold peers to hot).
    #[must_use]
    pub fn get_swarm_peer(&self, overlay: &OverlayAddress) -> Option<SwarmPeer> {
        self.get_or_load(overlay).map(|e| e.swarm_peer())
    }

    /// Get a snapshot of all banned peer overlays.
    #[must_use]
    pub fn banned_set(&self) -> std::collections::HashSet<OverlayAddress> {
        self.banned_set.iter().map(|r| *r).collect()
    }

    /// Get a snapshot of all hot peers currently in backoff.
    #[must_use]
    pub fn peers_in_backoff(&self) -> std::collections::HashSet<OverlayAddress> {
        self.peers
            .iter()
            .filter(|r| r.value().is_in_backoff())
            .map(|r| *r.key())
            .collect()
    }

    /// Check if peer is in backoff via hot cache.
    #[must_use]
    pub fn peer_is_in_backoff(&self, overlay: &OverlayAddress) -> bool {
        self.peers.get(overlay).is_some_and(|e| e.is_in_backoff())
    }

    /// Check if peer is banned (O(1) via DashSet).
    #[must_use]
    pub fn is_banned(&self, overlay: &OverlayAddress) -> bool {
        self.banned_set.contains(overlay)
    }

    /// Number of currently banned peers (O(1)).
    #[must_use]
    pub fn banned_count(&self) -> usize {
        self.banned_set.len()
    }

    /// Ban a peer (prevents dialing). Notifies ban subscribers for disconnect.
    ///
    /// Bans are written directly to DB (bypassing the write buffer) for immediacy.
    pub fn ban(&self, overlay: &OverlayAddress, reason: Option<String>) {
        if !self.banned_set.insert(*overlay) {
            return; // Already banned
        }

        gauge!("peer_manager_banned_peers").increment(1.0);

        // Update hot peer entry if present
        if let Some(entry) = self.peers.get(overlay) {
            if !entry.is_banned() {
                let old_state = entry.health_state();
                warn!(?overlay, ?reason, "banning peer");
                entry.ban(reason.clone());
                on_health_changed(old_state, HealthState::Banned);
            }
        } else {
            warn!(?overlay, ?reason, "banning cold peer");
        }

        // Write ban directly to DB (bypass buffer for immediacy)
        if let Some(ref store) = self.store
            && let Ok(Some(mut record)) = store.get(overlay)
        {
            record.ban_info = Some((unix_timestamp_secs(), reason.unwrap_or_default()));
            let _ = store.save(&record);
        }

        let _ = self.ban_tx.send(*overlay);
    }

    /// Subscribe to ban notifications for disconnecting banned peers.
    pub fn subscribe_bans(&self) -> broadcast::Receiver<OverlayAddress> {
        self.ban_tx.subscribe()
    }

    /// Store a single discovered peer.
    ///
    /// For hot peers, updates addresses (preserves handshake-confirmed node_type).
    /// For cold peers, just touches the LRU index.
    /// For new peers, adds to index and buffers a write to DB.
    /// Without a store, inserts directly into DashMap.
    pub fn store_discovered_peer(&self, swarm_peer: SwarmPeer) -> OverlayAddress {
        let overlay = OverlayAddress::from(*swarm_peer.overlay());
        if let Some(entry) = self.peers.get(&overlay) {
            // Hot peer - update addresses (dirty flag set by entry)
            entry.update_addresses(swarm_peer);
            self.index.touch(&overlay);
        } else if self.store.is_some() {
            match self.index.add(overlay) {
                Ok(()) => {
                    // Truly new peer - buffer initial record to DB
                    gauge!("peer_manager_total_peers").increment(1.0);
                    let record = StoredPeer::new_discovered(swarm_peer);
                    if self.write_buffer.push(record) {
                        self.flush_write_buffer();
                    }
                }
                Err(AddError::AlreadyPresent) => {
                    // Known cold peer - just touch LRU
                    self.index.touch(&overlay);
                }
                Err(AddError::BinFull) => {
                    // Bin at capacity - save to DB only, don't add to index
                    let record = StoredPeer::new_discovered(swarm_peer);
                    if self.write_buffer.push(record) {
                        self.flush_write_buffer();
                    }
                }
            }
        } else {
            // No store - insert into DashMap (backward compat)
            self.insert_peer(overlay, swarm_peer, SwarmNodeType::Client);
        }
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
            stored_overlays.push(self.store_discovered_peer(swarm_peer));
        }
        stored_overlays
    }

    /// Called when a peer completes handshake. Always inserts into hot cache.
    pub fn on_peer_ready(&self, swarm_peer: SwarmPeer, node_type: SwarmNodeType) {
        let overlay = OverlayAddress::from(*swarm_peer.overlay());
        debug!(?overlay, ?node_type, "storing peer");

        self.insert_peer(overlay, swarm_peer, node_type);
        if let Some(entry) = self.peers.get(&overlay) {
            let old_state = entry.health_state();
            entry.record_success(Duration::ZERO);
            on_health_changed(old_state, entry.health_state());
        }
    }

    /// Total peers persisted in the backing store (or hot cache size if no store).
    #[must_use]
    pub fn stored_count(&self) -> usize {
        match self.store {
            Some(ref store) => store.count().unwrap_or(0),
            None => self.peers.len(),
        }
    }

    /// Check DashMap first, then load from DB and promote to hot cache.
    fn get_or_load(&self, overlay: &OverlayAddress) -> Option<Arc<PeerEntry>> {
        if let Some(entry) = self.peers.get(overlay) {
            return Some(Arc::clone(entry.value()));
        }

        let store = self.store.as_ref()?;
        let record = store.get(overlay).ok()??;
        let score = self
            .score_store
            .as_ref()
            .and_then(|ss| ss.load(overlay).ok().flatten());

        use dashmap::mapref::entry::Entry;
        match self.peers.entry(*overlay) {
            Entry::Occupied(e) => Some(Arc::clone(e.get())),
            Entry::Vacant(e) => {
                let entry = Arc::new(PeerEntry::from_record(
                    record,
                    score,
                    Arc::clone(&self.scoring_config),
                    Arc::clone(&self.callbacks),
                ));
                let cloned = Arc::clone(&entry);
                self.score_distribution.on_peer_added(cloned.score());
                on_health_added(cloned.health_state());
                e.insert(entry);
                gauge!("peer_manager_hot_peers").set(self.peers.len() as f64);
                Some(cloned)
            }
        }
    }

    /// Snapshot a peer entry into the write buffer (record + score).
    pub(crate) fn buffer_entry(&self, overlay: OverlayAddress, entry: &PeerEntry) {
        let record = StoredPeer::from(entry);
        self.write_buffer.push(record);
        if self.score_store.is_some() {
            self.write_buffer
                .push_score(overlay, entry.score_snapshot());
        }
    }

    /// Check if a cold peer is dialable from its stored record.
    fn is_cold_peer_dialable(&self, overlay: &OverlayAddress) -> bool {
        let Some(ref store) = self.store else {
            return false;
        };
        let Ok(Some(record)) = store.get(overlay) else {
            return false;
        };
        record.is_dialable()
    }

    /// Insert or update a peer in the hot cache.
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
                    Arc::clone(&self.callbacks),
                ));
                let initial_score = entry.score();
                e.insert(entry);
                if self.index.add(overlay).is_ok() {
                    gauge!("peer_manager_total_peers").increment(1.0);
                }
                gauge!("peer_manager_hot_peers").set(self.peers.len() as f64);
                self.score_distribution.on_peer_added(initial_score);
                on_health_added(HealthState::Healthy);
            }
        }
    }

    /// Fully remove a peer from all data structures (index, hot cache, DB, banned set).
    pub(crate) fn remove_peer(&self, overlay: &OverlayAddress) {
        let was_hot = if let Some((_, entry)) = self.peers.remove(overlay) {
            self.score_distribution.on_peer_removed(entry.score());
            on_health_removed(entry.health_state());
            true
        } else {
            false
        };
        if self.index.remove(overlay) {
            gauge!("peer_manager_total_peers").decrement(1.0);
        }
        if was_hot {
            gauge!("peer_manager_hot_peers").set(self.peers.len() as f64);
        }
        if self.banned_set.remove(overlay).is_some() {
            gauge!("peer_manager_banned_peers").decrement(1.0);
        }
        if let Some(ref store) = self.store {
            let _ = store.remove(overlay);
        }
    }
}

impl<I: SwarmIdentity> SwarmPeerResolver for PeerManager<I> {
    type Peer = SwarmPeer;

    fn get_swarm_peer(&self, overlay: &OverlayAddress) -> Option<SwarmPeer> {
        self.get_or_load(overlay).map(|e| e.swarm_peer())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_net_peer_store::MemoryPeerStore;
    use vertex_swarm_api::SwarmScoreStore;
    use vertex_swarm_test_utils::{MockIdentity, test_overlay, test_swarm_peer};

    fn mock_identity() -> MockIdentity {
        MockIdentity::with_overlay(test_overlay(0))
    }

    #[test]
    fn test_store_discovered_peer() {
        let pm = PeerManager::new(&mock_identity());
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        let stored = pm.store_discovered_peer(swarm_peer.clone());
        assert_eq!(stored, overlay);
        assert!(pm.get_swarm_peer(&overlay).is_some());
        assert!(pm.index().exists(&overlay));
    }

    #[test]
    fn test_on_peer_ready() {
        let pm = PeerManager::new(&mock_identity());
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        pm.on_peer_ready(swarm_peer, SwarmNodeType::Storer);
        assert_eq!(pm.node_type(&overlay), Some(SwarmNodeType::Storer));
        assert!(pm.index().exists(&overlay));
    }

    #[test]
    fn test_peer_lifecycle() {
        let pm = PeerManager::new(&mock_identity());
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        pm.store_discovered_peer(swarm_peer.clone());
        assert!(pm.eligible_peers().contains(&overlay));

        pm.on_peer_ready(swarm_peer, SwarmNodeType::Client);
        assert!(pm.eligible_peers().contains(&overlay));
    }

    #[test]
    fn test_ban() {
        let pm = PeerManager::new(&mock_identity());
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        pm.on_peer_ready(swarm_peer, SwarmNodeType::Client);
        pm.ban(&overlay, Some("misbehaving".to_string()));

        assert!(pm.is_banned(&overlay));
        assert!(!pm.eligible_peers().contains(&overlay));
    }

    #[test]
    fn test_get_dialable_peers() {
        let pm = PeerManager::new(&mock_identity());

        for n in 1..=5 {
            pm.store_discovered_peer(test_swarm_peer(n));
        }

        pm.ban(&test_overlay(1), None);

        let all_overlays: Vec<_> = (1..=5).map(test_overlay).collect();
        let dialable = pm.get_dialable_peers(&all_overlays);

        assert_eq!(dialable.len(), 4);
    }

    #[test]
    fn test_custom_scoring_config() {
        let config = SwarmScoringConfig::lenient();
        let pm = PeerManager::with_config(&mock_identity(), config, DEFAULT_MAX_PER_BIN);

        assert!(pm.scoring_config.ban_threshold() < 0.0);
    }

    #[test]
    fn test_known_storer_overlays() {
        let pm = PeerManager::new(&mock_identity());

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
        let pm = PeerManager::new(&mock_identity());

        for n in 1..=5 {
            pm.on_peer_ready(test_swarm_peer(n), SwarmNodeType::Storer);
        }

        let overlays = vec![test_overlay(1), test_overlay(3), test_overlay(5)];
        let peers = pm.get_swarm_peers(&overlays);

        assert_eq!(peers.len(), 3);
    }

    #[test]
    fn test_get_swarm_peers_missing() {
        let pm = PeerManager::new(&mock_identity());

        pm.on_peer_ready(test_swarm_peer(1), SwarmNodeType::Storer);

        let overlays = vec![test_overlay(1), test_overlay(99)];
        let peers = pm.get_swarm_peers(&overlays);

        assert_eq!(peers.len(), 1);
    }

    #[test]
    fn test_node_type_variants() {
        let pm = PeerManager::new(&mock_identity());

        pm.on_peer_ready(test_swarm_peer(1), SwarmNodeType::Bootnode);
        pm.on_peer_ready(test_swarm_peer(2), SwarmNodeType::Client);
        pm.on_peer_ready(test_swarm_peer(3), SwarmNodeType::Storer);

        assert_eq!(
            pm.node_type(&test_overlay(1)),
            Some(SwarmNodeType::Bootnode)
        );
        assert_eq!(pm.node_type(&test_overlay(2)), Some(SwarmNodeType::Client));
        assert_eq!(pm.node_type(&test_overlay(3)), Some(SwarmNodeType::Storer));
    }

    #[test]
    fn test_bin_index_integration() {
        let pm = PeerManager::new(&mock_identity());

        for n in 1..=5 {
            pm.store_discovered_peer(test_swarm_peer(n));
        }

        let bin_sizes = pm.index().bin_sizes();
        let total: usize = bin_sizes.iter().sum();
        assert_eq!(total, 5);

        for n in 1..=5 {
            assert!(pm.index().exists(&test_overlay(n)));
        }
    }

    #[test]
    fn test_lru_ordering_preserved() {
        let pm = PeerManager::new(&mock_identity());

        pm.store_discovered_peer(test_swarm_peer(1));
        pm.store_discovered_peer(test_swarm_peer(2));
        pm.store_discovered_peer(test_swarm_peer(3));

        pm.store_discovered_peer(test_swarm_peer(1));
    }

    #[test]
    fn test_dialable_in_bin() {
        let pm = PeerManager::new(&mock_identity());

        let p1 = OverlayAddress::from([0x80; 32]);
        let p2 = OverlayAddress::from([0xc0; 32]);
        let p3 = OverlayAddress::from([0xa0; 32]);

        let peer1 = test_swarm_peer(1);
        let peer2 = test_swarm_peer(2);
        let peer3 = test_swarm_peer(3);

        let _ = pm.index.add(p1);
        let _ = pm.index.add(p2);
        let _ = pm.index.add(p3);
        pm.peers.insert(
            p1,
            Arc::new(PeerEntry::with_config(
                peer1,
                SwarmNodeType::Client,
                p1,
                Arc::clone(&pm.scoring_config),
                Arc::clone(&pm.callbacks),
            )),
        );
        pm.peers.insert(
            p2,
            Arc::new(PeerEntry::with_config(
                peer2,
                SwarmNodeType::Client,
                p2,
                Arc::clone(&pm.scoring_config),
                Arc::clone(&pm.callbacks),
            )),
        );
        pm.peers.insert(
            p3,
            Arc::new(PeerEntry::with_config(
                peer3,
                SwarmNodeType::Client,
                p3,
                Arc::clone(&pm.scoring_config),
                Arc::clone(&pm.callbacks),
            )),
        );

        pm.ban(&p1, None);

        let dialable = pm.dialable_in_bin(0, 2);
        assert_eq!(dialable.len(), 2);
    }

    #[test]
    fn test_get_swarm_peer() {
        let pm = PeerManager::new(&mock_identity());
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        assert!(pm.get_swarm_peer(&overlay).is_none());
        pm.store_discovered_peer(swarm_peer.clone());
        assert!(pm.get_swarm_peer(&overlay).is_some());
    }

    #[test]
    fn test_persistence_roundtrip() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> =
            Arc::new(MemoryPeerStore::<StoredPeer>::new());

        let pm1 = PeerManager::with_store(
            &mock_identity(),
            store.clone(),
            None,
            SwarmScoringConfig::default(),
            DEFAULT_MAX_PER_BIN,
        );

        for n in 1..=5 {
            pm1.on_peer_ready(test_swarm_peer(n), SwarmNodeType::Storer);
        }
        pm1.ban(&test_overlay(1), Some("bad".to_string()));
        pm1.collect_dirty();
        pm1.flush_write_buffer();

        let pm2 = PeerManager::with_store(
            &mock_identity(),
            store,
            None,
            SwarmScoringConfig::default(),
            DEFAULT_MAX_PER_BIN,
        );
        assert_eq!(pm2.index().len(), 5);
        assert!(pm2.is_banned(&test_overlay(1)));
        assert!(!pm2.is_banned(&test_overlay(2)));
    }

    #[test]
    fn test_store_discovered_peer_preserves_node_type() {
        let pm = PeerManager::new(&mock_identity());
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        pm.on_peer_ready(swarm_peer.clone(), SwarmNodeType::Storer);
        assert_eq!(pm.node_type(&overlay), Some(SwarmNodeType::Storer));

        pm.store_discovered_peer(swarm_peer);
        assert_eq!(pm.node_type(&overlay), Some(SwarmNodeType::Storer));
    }

    #[test]
    fn test_store_discovered_peer_defaults_to_client() {
        let pm = PeerManager::new(&mock_identity());
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        pm.store_discovered_peer(swarm_peer);
        assert_eq!(pm.node_type(&overlay), Some(SwarmNodeType::Client));
    }

    #[test]
    fn test_store_discovered_peers_preserves_node_type() {
        let pm = PeerManager::new(&mock_identity());

        pm.on_peer_ready(test_swarm_peer(1), SwarmNodeType::Storer);
        pm.on_peer_ready(test_swarm_peer(2), SwarmNodeType::Storer);

        let peers = vec![test_swarm_peer(1), test_swarm_peer(2), test_swarm_peer(3)];
        pm.store_discovered_peers(peers);

        assert_eq!(pm.node_type(&test_overlay(1)), Some(SwarmNodeType::Storer));
        assert_eq!(pm.node_type(&test_overlay(2)), Some(SwarmNodeType::Storer));
        assert_eq!(pm.node_type(&test_overlay(3)), Some(SwarmNodeType::Client));
    }

    #[test]
    fn test_banned_count_tracking() {
        let pm = PeerManager::new(&mock_identity());

        for n in 1..=5 {
            pm.store_discovered_peer(test_swarm_peer(n));
        }

        assert_eq!(pm.eligible_count(), 5);
        pm.ban(&test_overlay(1), None);
        assert_eq!(pm.eligible_count(), 4);
        pm.ban(&test_overlay(2), None);
        assert_eq!(pm.eligible_count(), 3);

        pm.ban(&test_overlay(1), None);
        assert_eq!(pm.eligible_count(), 3);
    }

    #[test]
    fn test_gossip_peers_cold() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> =
            Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = PeerManager::with_store(
            &mock_identity(),
            store.clone(),
            None,
            SwarmScoringConfig::default(),
            DEFAULT_MAX_PER_BIN,
        );

        pm.store_discovered_peer(test_swarm_peer(1));
        pm.store_discovered_peer(test_swarm_peer(2));

        assert_eq!(pm.index().len(), 2);
        assert_eq!(pm.peers.len(), 0);
    }

    #[test]
    fn test_connected_peers_always_hot() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> =
            Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = PeerManager::with_store(
            &mock_identity(),
            store,
            None,
            SwarmScoringConfig::default(),
            DEFAULT_MAX_PER_BIN,
        );

        pm.on_peer_ready(test_swarm_peer(1), SwarmNodeType::Client);

        assert_eq!(pm.index().len(), 1);
        assert_eq!(pm.peers.len(), 1);
    }

    #[test]
    fn test_get_or_load_promotes() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> =
            Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = PeerManager::with_store(
            &mock_identity(),
            store.clone(),
            None,
            SwarmScoringConfig::default(),
            DEFAULT_MAX_PER_BIN,
        );

        pm.on_peer_ready(test_swarm_peer(1), SwarmNodeType::Storer);
        pm.collect_dirty();
        pm.flush_write_buffer();

        let pm2 = PeerManager::with_store(
            &mock_identity(),
            store,
            None,
            SwarmScoringConfig::default(),
            DEFAULT_MAX_PER_BIN,
        );
        assert_eq!(pm2.peers.len(), 0);

        let peer = pm2.get_swarm_peer(&test_overlay(1));
        assert!(peer.is_some());
        assert_eq!(pm2.peers.len(), 1);
    }

    #[test]
    fn test_banned_set_o1() {
        let pm = PeerManager::new(&mock_identity());

        pm.on_peer_ready(test_swarm_peer(1), SwarmNodeType::Client);
        pm.ban(&test_overlay(1), None);

        assert!(pm.is_banned(&test_overlay(1)));
        assert!(!pm.is_banned(&test_overlay(2)));
        assert_eq!(pm.banned_count(), 1);
    }

    #[test]
    fn test_write_buffer_flush() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> =
            Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = PeerManager::with_store(
            &mock_identity(),
            store.clone(),
            None,
            SwarmScoringConfig::default(),
            DEFAULT_MAX_PER_BIN,
        );

        pm.on_peer_ready(test_swarm_peer(1), SwarmNodeType::Client);
        pm.collect_dirty();
        pm.flush_write_buffer();

        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn test_ban_bypasses_buffer() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> =
            Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = PeerManager::with_store(
            &mock_identity(),
            store.clone(),
            None,
            SwarmScoringConfig::default(),
            DEFAULT_MAX_PER_BIN,
        );

        pm.on_peer_ready(test_swarm_peer(1), SwarmNodeType::Client);
        pm.collect_dirty();
        pm.flush_write_buffer();

        pm.ban(&test_overlay(1), Some("test".into()));

        let record = store.get(&test_overlay(1)).unwrap().unwrap();
        assert!(record.ban_info.is_some());
    }

    #[test]
    fn test_eligible_count_o1() {
        let pm = PeerManager::new(&mock_identity());

        for n in 1..=10 {
            pm.store_discovered_peer(test_swarm_peer(n));
        }

        assert_eq!(pm.eligible_count(), 10);
        pm.ban(&test_overlay(1), None);
        pm.ban(&test_overlay(2), None);
        assert_eq!(pm.eligible_count(), 8);
    }

    #[test]
    fn test_db_roundtrip_hot_cold() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> =
            Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = PeerManager::with_store(
            &mock_identity(),
            store.clone(),
            None,
            SwarmScoringConfig::default(),
            DEFAULT_MAX_PER_BIN,
        );

        pm.store_discovered_peer(test_swarm_peer(1));
        assert_eq!(pm.peers.len(), 0);

        pm.on_peer_ready(test_swarm_peer(1), SwarmNodeType::Storer);
        assert_eq!(pm.peers.len(), 1);

        pm.collect_dirty();
        pm.flush_write_buffer();

        let pm2 = PeerManager::with_store(
            &mock_identity(),
            store,
            None,
            SwarmScoringConfig::default(),
            DEFAULT_MAX_PER_BIN,
        );
        assert_eq!(pm2.index().len(), 1);
        assert_eq!(pm2.peers.len(), 0);

        pm2.store_discovered_peer(test_swarm_peer(1));
        assert_eq!(pm2.peers.len(), 0);
    }

    #[test]
    fn test_evict_cold() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> =
            Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = PeerManager::build(
            &mock_identity(),
            SwarmScoringConfig::default(),
            DEFAULT_MAX_PER_BIN,
            Some(store),
            None,
            10,
        );

        for n in 1..=20 {
            pm.on_peer_ready(test_swarm_peer(n), SwarmNodeType::Client);
        }
        assert_eq!(pm.peers.len(), 20);

        for n in 1..=15 {
            pm.record_dial_failure(&test_overlay(n));
        }

        pm.evict_cold();

        assert_eq!(pm.peers.len(), 10);

        for n in 16..=20 {
            assert!(pm.peers.contains_key(&test_overlay(n)));
        }
    }

    #[test]
    fn test_concurrent_hot_cold_access() {
        use std::thread;

        let store: Arc<dyn NetPeerStore<StoredPeer>> =
            Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = PeerManager::with_store(
            &mock_identity(),
            store,
            None,
            SwarmScoringConfig::default(),
            DEFAULT_MAX_PER_BIN,
        );
        let pm = Arc::new(pm);

        let mut handles = vec![];

        for batch in 0..4 {
            let pm = Arc::clone(&pm);
            handles.push(thread::spawn(move || {
                for i in 0..25 {
                    pm.store_discovered_peer(test_swarm_peer((batch * 25 + i + 1) as u8));
                }
            }));
        }

        {
            let pm = Arc::clone(&pm);
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    let _ = pm.eligible_count();
                    let _ = pm.is_banned(&test_overlay(1));
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(pm.index().len(), 100);
    }

    #[test]
    fn test_discover_flush_reload() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> =
            Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = PeerManager::with_store(
            &mock_identity(),
            store.clone(),
            None,
            SwarmScoringConfig::default(),
            DEFAULT_MAX_PER_BIN,
        );

        for n in 1..=10 {
            pm.store_discovered_peer(test_swarm_peer(n));
        }
        assert_eq!(pm.index().len(), 10);
        assert_eq!(pm.peers.len(), 0);

        pm.flush_write_buffer();
        assert_eq!(store.count().unwrap(), 10);

        let pm2 = PeerManager::with_store(
            &mock_identity(),
            store,
            None,
            SwarmScoringConfig::default(),
            DEFAULT_MAX_PER_BIN,
        );
        assert_eq!(pm2.index().len(), 10);
        assert_eq!(pm2.peers.len(), 0);

        for n in 1..=10 {
            assert!(pm2.get_swarm_peer(&test_overlay(n)).is_some());
        }
        assert_eq!(pm2.peers.len(), 10);
    }

    #[test]
    fn test_full_lifecycle() {
        let db = vertex_storage_redb::RedbDatabase::in_memory()
            .unwrap()
            .into_arc();
        let db_store = Arc::new(crate::db_store::DbPeerStore::new(Arc::clone(&db)));
        db_store.init().unwrap();
        let store: Arc<dyn NetPeerStore<StoredPeer>> = Arc::clone(&db_store) as _;
        let score_store: Option<
            Arc<
                dyn SwarmScoreStore<
                        Value = PeerScore,
                        Error = vertex_net_peer_store::error::StoreError,
                    >,
            >,
        > = Some(Arc::clone(&db_store) as _);
        let pm = PeerManager::with_store(
            &mock_identity(),
            store.clone(),
            score_store.clone(),
            SwarmScoringConfig::default(),
            DEFAULT_MAX_PER_BIN,
        );

        pm.store_discovered_peer(test_swarm_peer(1));
        assert_eq!(pm.peers.len(), 0);

        pm.on_peer_ready(test_swarm_peer(1), SwarmNodeType::Storer);
        assert_eq!(pm.peers.len(), 1);

        {
            let entry = pm.peers.get(&test_overlay(1)).unwrap();
            entry.record_success(Duration::from_millis(50));
            assert!(entry.score() > 0.0);
        }

        pm.collect_dirty();
        pm.flush_write_buffer();

        let pm2 = PeerManager::with_store(
            &mock_identity(),
            store,
            score_store,
            SwarmScoringConfig::default(),
            DEFAULT_MAX_PER_BIN,
        );
        assert_eq!(pm2.index().len(), 1);
        assert_eq!(pm2.peers.len(), 0);

        let peer = pm2.get_swarm_peer(&test_overlay(1));
        assert!(peer.is_some());
        let entry = pm2.peers.get(&test_overlay(1)).unwrap();
        assert!(entry.score() > 0.0);
        assert_eq!(entry.node_type(), SwarmNodeType::Storer);
    }

    #[test]
    fn test_ban_persistence() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> =
            Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = PeerManager::with_store(
            &mock_identity(),
            store.clone(),
            None,
            SwarmScoringConfig::default(),
            DEFAULT_MAX_PER_BIN,
        );

        pm.on_peer_ready(test_swarm_peer(1), SwarmNodeType::Client);
        pm.collect_dirty();
        pm.flush_write_buffer();
        pm.ban(&test_overlay(1), Some("test ban".into()));

        let record = store.get(&test_overlay(1)).unwrap().unwrap();
        assert!(record.ban_info.is_some());

        let pm2 = PeerManager::with_store(
            &mock_identity(),
            store,
            None,
            SwarmScoringConfig::default(),
            DEFAULT_MAX_PER_BIN,
        );
        assert!(pm2.is_banned(&test_overlay(1)));
        assert!(!pm2.eligible_peers().contains(&test_overlay(1)));
    }

    #[test]
    fn test_concurrent_gossip_and_queries() {
        use std::thread;

        let store: Arc<dyn NetPeerStore<StoredPeer>> =
            Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = PeerManager::with_store(
            &mock_identity(),
            store,
            None,
            SwarmScoringConfig::default(),
            DEFAULT_MAX_PER_BIN,
        );
        let pm = Arc::new(pm);

        let mut handles = vec![];

        for batch in 0..4 {
            let pm = Arc::clone(&pm);
            handles.push(thread::spawn(move || {
                for i in 0..25 {
                    let n = (batch * 25 + i + 1) as u8;
                    pm.store_discovered_peer(test_swarm_peer(n));
                }
            }));
        }

        for _ in 0..2 {
            let pm = Arc::clone(&pm);
            handles.push(thread::spawn(move || {
                for _ in 0..50 {
                    let _ = pm.eligible_count();
                    let _ = pm.eligible_peers();
                    let _ = pm.is_banned(&test_overlay(1));
                }
            }));
        }

        {
            let pm = Arc::clone(&pm);
            handles.push(thread::spawn(move || {
                for _ in 0..10 {
                    pm.flush_write_buffer();
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(pm.index().len(), 100);
    }

    #[test]
    fn test_memory_bounded() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> =
            Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = PeerManager::with_store(
            &mock_identity(),
            store,
            None,
            SwarmScoringConfig::default(),
            DEFAULT_MAX_PER_BIN,
        );

        for n in 1..=200 {
            pm.store_discovered_peer(test_swarm_peer(n));
        }

        assert_eq!(pm.index().len(), 200);
        assert_eq!(pm.peers.len(), 0);
        assert_eq!(pm.eligible_count(), 200);
    }
}
