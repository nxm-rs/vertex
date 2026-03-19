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
use tracing::{debug, trace, warn};
use vertex_net_local::IpCapability;
use vertex_net_peer_store::{NetPeerStore, NetRecord, StoreError};
use vertex_swarm_api::{SwarmIdentity, SwarmPeerResolver, SwarmScoreStore, SwarmSpec};
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_peer_score::{PeerScore, ScoreCallbacks, SwarmScoringConfig, SwarmScoringEvent};
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
    _identity: PhantomData<I>,
    /// In-memory peer index with LRU ordering (ALL known overlays).
    index: ProximityIndex,
    /// Hot cache: connected + recently-accessed peers.
    peers: DashMap<OverlayAddress, Arc<PeerEntry>>,
    /// Database backend for cold storage (None for ephemeral/test mode).
    store: Option<Arc<dyn NetPeerStore<StoredPeer>>>,
    /// Score persistence (None for ephemeral/test mode).
    score_store: Option<Arc<dyn SwarmScoreStore<Score = PeerScore, Error = StoreError>>>,
    /// O(1) ban checks without DB or DashMap lookup.
    banned_set: DashSet<OverlayAddress>,
    /// Batches DB writes for amortized flush.
    write_buffer: WriteBuffer,
    /// Scoring configuration.
    scoring_config: Arc<SwarmScoringConfig>,
    /// Maximum peers in hot DashMap cache before eviction.
    max_hot_peers: usize,
    /// Callbacks shared with all PeerEntries.
    callbacks: Arc<ScoreCallbacks>,
    /// Per-bucket gauge tracking of score distribution.
    score_distribution: Arc<ScoreDistribution>,
    /// Channel for notifying topology of banned peers.
    ban_tx: broadcast::Sender<OverlayAddress>,
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
        Self::build(identity, scoring_config, max_per_bin, None, None, DEFAULT_MAX_HOT_PEERS)
    }

    /// Create with a database-backed persistent store.
    ///
    /// Loads the overlay index and banned set from the store on construction.
    /// Hot cache starts empty; peers are promoted on access.
    /// Scores are loaded lazily when peers are promoted to the hot cache.
    pub fn with_store(
        identity: &I,
        store: Arc<dyn NetPeerStore<StoredPeer>>,
        score_store: Option<Arc<dyn SwarmScoreStore<Score = PeerScore, Error = StoreError>>>,
        scoring_config: SwarmScoringConfig,
        max_per_bin: usize,
    ) -> Arc<Self> {
        let pm = Self::build(identity, scoring_config, max_per_bin, Some(store), score_store, DEFAULT_MAX_HOT_PEERS);
        pm.load_index_from_store();
        pm
    }

    fn build(
        identity: &I,
        scoring_config: SwarmScoringConfig,
        max_per_bin: usize,
        store: Option<Arc<dyn NetPeerStore<StoredPeer>>>,
        score_store: Option<Arc<dyn SwarmScoreStore<Score = PeerScore, Error = StoreError>>>,
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
            return self.peers
                .iter()
                .filter(|r| r.value().is_dialable())
                .map(|r| *r.key())
                .collect();
        }
        self.index.all_peers()
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
        // Two-phase filter: fast checks under the bin read lock, DB reads outside.
        // This avoids holding the ProximityIndex bin lock during blocking I/O.
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
                return false; // Don't add to filter_bin result
            }
            // Cold peer — defer DB check
            cold_candidates.push(*overlay);
            false
        });

        // Phase 2: check cold peers outside any lock.
        if result.len() < count {
            if let Some(ref store) = self.store {
                for overlay in &cold_candidates {
                    if result.len() >= count {
                        break;
                    }
                    if let Ok(Some(record)) = store.get(overlay) {
                        if record.node_type == SwarmNodeType::Storer && !record.is_banned() {
                            result.push(*overlay);
                        }
                    }
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
    ///
    /// Hot peers are checked via DashMap under the bin lock. Cold peers are
    /// checked via DB outside the lock to avoid blocking bin writes during I/O.
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

    pub fn record_latency(&self, overlay: &OverlayAddress, rtt: Duration) {
        if let Some(entry) = self.peers.get(overlay) {
            entry.set_latency(rtt);
            trace!(?overlay, ?rtt, "recorded latency");
        }
    }

    pub fn record_dial_failure(&self, overlay: &OverlayAddress) {
        if let Some(entry) = self.peers.get(overlay) {
            let old_state = entry.health_state();
            entry.record_dial_failure();
            on_health_changed(old_state, entry.health_state());
            let failures = entry.consecutive_failures();
            let backoff = entry.backoff_remaining();
            debug!(
                ?overlay,
                failures,
                backoff_secs = backoff.map(|d| d.as_secs()),
                "recorded dial failure with backoff"
            );
        } else if let Some(ref store) = self.store {
            // Cold peer - load, modify, route through write buffer
            if let Ok(Some(mut record)) = store.get(overlay) {
                record.consecutive_failures += 1;
                record.last_dial_attempt = unix_timestamp_secs();
                if self.write_buffer.push(record) {
                    self.flush_write_buffer();
                }
            }
        }
    }

    /// Record an early disconnect for a peer (post-handshake connection that failed quickly).
    pub fn record_early_disconnect(&self, overlay: &OverlayAddress, duration: Duration) {
        if let Some(entry) = self.peers.get(overlay) {
            let old_state = entry.health_state();
            entry.record_early_disconnect(duration);
            on_health_changed(old_state, entry.health_state());
            let failures = entry.consecutive_failures();
            let backoff = entry.backoff_remaining();
            debug!(
                ?overlay,
                ?duration,
                failures,
                backoff_secs = backoff.map(|d| d.as_secs()),
                "recorded early disconnect with backoff"
            );
        }
    }

    /// Record a scoring event for a peer.
    pub fn record_scoring_event(&self, overlay: &OverlayAddress, event: SwarmScoringEvent) {
        if let Some(entry) = self.peers.get(overlay) {
            entry.record_event(event);
        }
    }

    /// Remove stale peers unconditionally.
    pub fn purge_stale(&self) {
        let stale: Vec<OverlayAddress> = self
            .peers
            .iter()
            .filter(|r| r.value().is_stale())
            .map(|r| *r.key())
            .collect();

        if stale.is_empty() {
            return;
        }

        for overlay in &stale {
            self.remove_peer(overlay);
        }

        debug!(removed = stale.len(), remaining = self.index.len(), "purged stale peers");
    }

    /// Collect dirty hot peers into the write buffer for batched DB flush.
    pub fn collect_dirty(&self) {
        if self.store.is_none() {
            return;
        }
        for entry in self.peers.iter() {
            if entry.value().take_dirty() {
                self.buffer_entry(*entry.key(), entry.value());
            }
        }
    }

    /// Flush the write buffer to the DB (peer records and scores).
    pub fn flush_write_buffer(&self) {
        if let Some(ref store) = self.store
            && let Err(e) = self.write_buffer.flush(store.as_ref())
        {
            warn!(error = %e, "failed to flush write buffer");
        }
        if let Some(ref ss) = self.score_store {
            let scores = self.write_buffer.drain_scores();
            if !scores.is_empty() {
                if let Err(e) = ss.save_score_batch(&scores) {
                    warn!(error = %e, "failed to flush score buffer");
                }
            }
        }
    }

    /// Evict non-connected peers from the hot cache to keep it bounded.
    ///
    /// Peers with consecutive failures > 0 are considered disconnected and
    /// eligible for eviction. Their state is saved to DB before removal.
    pub fn evict_cold(&self) {
        if self.store.is_none() {
            return;
        }
        let current = self.peers.len();
        if current <= self.max_hot_peers {
            return;
        }

        let to_evict = current.saturating_sub(self.max_hot_peers);

        // Collect eviction candidates: peers with failures (not connected)
        let mut candidates: Vec<(OverlayAddress, u64)> = self.peers
            .iter()
            .filter(|r| r.value().consecutive_failures() > 0)
            .map(|r| (*r.key(), r.value().last_seen()))
            .collect();

        // Sort by last_seen ascending (oldest first)
        candidates.sort_unstable_by_key(|&(_, last_seen)| last_seen);

        let mut evicted = 0;
        for (overlay, _) in candidates.into_iter().take(to_evict) {
            // Remove from hot cache and snapshot to DB in one lookup
            if let Some((_, entry)) = self.peers.remove(&overlay) {
                self.buffer_entry(overlay, &entry);
                self.score_distribution.on_peer_removed(entry.score());
                on_health_removed(entry.health_state());
            }
            evicted += 1;
        }

        if evicted > 0 {
            self.flush_write_buffer();
            gauge!("peer_manager_hot_peers").set(self.peers.len() as f64);
            debug!(evicted, remaining_hot = self.peers.len(), "evicted cold peers from hot cache");
        }
    }

    /// Replenish depleted proximity bins from the database.
    ///
    /// Scans bins below half capacity (low-water mark), then streams DB keys
    /// to fill them. Runs periodically from the persistence task.
    pub fn replenish_bins(&self) {
        let Some(ref store) = self.store else { return };

        let max_per_bin = self.index.max_per_bin();
        if max_per_bin == 0 {
            return; // Unbounded index, nothing to replenish
        }

        // Build per-bin remaining capacity array for O(1) lookup.
        let low_water = max_per_bin / 2;
        let bin_sizes = self.index.bin_sizes();
        let max_po = self.index.max_po() as usize;
        let mut remaining = vec![0usize; max_po + 1];
        let mut any_depleted = false;
        for (po, &size) in bin_sizes.iter().enumerate() {
            if size < low_water {
                remaining[po] = max_per_bin.saturating_sub(size);
                any_depleted = true;
            }
        }
        if !any_depleted {
            return;
        }

        // Key-only scan: loads overlay addresses without deserializing values.
        let overlays = match store.load_ids() {
            Ok(ids) => ids,
            Err(e) => {
                warn!(error = %e, "failed to load peer IDs for bin replenishment");
                return;
            }
        };

        let mut added = 0usize;
        for overlay in &overlays {
            if self.index.exists(overlay) {
                continue;
            }
            let po = self.index.bin_for(overlay) as usize;
            if remaining[po] > 0 && self.index.add(*overlay).is_ok() {
                added += 1;
                remaining[po] -= 1;
            }
        }

        if added > 0 {
            gauge!("peer_manager_total_peers").set(self.index.len() as f64);
            debug!(added, "replenished depleted proximity bins from store");
        }
    }

    /// Load the overlay index and banned set from the store.
    ///
    /// Uses key-only scan for the overlay index (no value deserialization),
    /// then loads banned overlays separately.
    /// Called once during construction. Does NOT populate the DashMap;
    /// peers are loaded on demand via `get_or_load`.
    fn load_index_from_store(&self) {
        let Some(ref store) = self.store else { return };

        // Phase 1: Key-only scan for overlay index (no value deserialization).
        let overlays = match store.load_ids() {
            Ok(ids) => ids,
            Err(e) => {
                warn!(error = %e, "failed to load peer IDs from store");
                return;
            }
        };

        let total_stored = overlays.len();
        let mut indexed = 0;
        for overlay in &overlays {
            if self.index.add(*overlay).is_ok() {
                indexed += 1;
            }
        }

        // Phase 2: Load banned overlays (needs value deserialization for ban_info).
        let mut banned = 0;
        if let Some(ref ss) = self.score_store {
            match ss.load_banned_overlays() {
                Ok(banned_overlays) => {
                    for overlay in &banned_overlays {
                        self.banned_set.insert(*overlay);
                    }
                    banned = banned_overlays.len();
                }
                Err(e) => warn!(error = %e, "failed to load banned peers"),
            }
        } else {
            // No score store — fall back to full record scan for ban info.
            match store.load_all() {
                Ok(records) => {
                    for record in &records {
                        if record.is_banned() {
                            self.banned_set.insert(*record.id());
                            banned += 1;
                        }
                    }
                }
                Err(e) => warn!(error = %e, "failed to load ban info from store"),
            }
        }

        gauge!("peer_manager_total_peers").set(indexed as f64);
        gauge!("peer_manager_banned_peers").set(banned as f64);
        gauge!("peer_manager_hot_peers").set(0.0f64);
        gauge!("peer_manager_stored_peers").set(total_stored as f64);

        if total_stored > 0 {
            debug!(
                total_stored,
                indexed,
                banned,
                "loaded peer index from store"
            );
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
    ///
    /// Uses the DashMap entry API to prevent a TOCTOU race where two concurrent
    /// callers both miss the initial check, both load from DB, and the second
    /// insert silently overwrites the first (losing any mutations made to it).
    fn get_or_load(&self, overlay: &OverlayAddress) -> Option<Arc<PeerEntry>> {
        // Fast path: already in hot cache.
        if let Some(entry) = self.peers.get(overlay) {
            return Some(Arc::clone(entry.value()));
        }

        // Slow path: load from DB. This happens outside any lock, so two threads
        // can race here — the entry API below resolves the race.
        let store = self.store.as_ref()?;
        let record = store.get(overlay).ok()??;
        let score = self.score_store.as_ref()
            .and_then(|ss| ss.get_score(overlay).ok().flatten());

        use dashmap::mapref::entry::Entry;
        match self.peers.entry(*overlay) {
            Entry::Occupied(e) => {
                // Another thread promoted this peer while we were loading from DB.
                // Use their entry (which may already have mutations applied).
                Some(Arc::clone(e.get()))
            }
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
    fn buffer_entry(&self, overlay: OverlayAddress, entry: &PeerEntry) {
        let record = StoredPeer::from(entry);
        self.write_buffer.push(record);
        if self.score_store.is_some() {
            self.write_buffer.push_score(overlay, entry.score_snapshot());
        }
    }

    /// Check if a cold peer is dialable from its stored record.
    fn is_cold_peer_dialable(&self, overlay: &OverlayAddress) -> bool {
        let Some(ref store) = self.store else { return false };
        let Ok(Some(record)) = store.get(overlay) else { return false };
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
                // Only increment total gauge if truly new (not cold→hot promotion)
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
    fn remove_peer(&self, overlay: &OverlayAddress) {
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
    use vertex_swarm_api::SwarmScoreStore;
    use vertex_net_peer_store::MemoryPeerStore;
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

        // Discover peer via Hive
        pm.store_discovered_peer(swarm_peer.clone());
        assert!(pm.eligible_peers().contains(&overlay));

        // Store as connected peer
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

        // Verify custom config is accepted (ban threshold propagates)
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

        assert_eq!(pm.node_type(&test_overlay(1)), Some(SwarmNodeType::Bootnode));
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

        // Update peer 1 (should move to MRU)
        pm.store_discovered_peer(test_swarm_peer(1));

        // Peer 2 should now be LRU
        // (exact order depends on which bin they're in)
    }

    #[test]
    fn test_dialable_in_bin() {
        let pm = PeerManager::new(&mock_identity());

        // Add peers to same bin
        let p1 = OverlayAddress::from([0x80; 32]);
        let p2 = OverlayAddress::from([0xc0; 32]);
        let p3 = OverlayAddress::from([0xa0; 32]);

        let peer1 = test_swarm_peer(1);
        let peer2 = test_swarm_peer(2);
        let peer3 = test_swarm_peer(3);

        // Manually insert with specific overlays (direct field access for mutation)
        let _ = pm.index.add(p1);
        let _ = pm.index.add(p2);
        let _ = pm.index.add(p3);
        pm.peers.insert(p1, Arc::new(PeerEntry::with_config(
            peer1, SwarmNodeType::Client, p1,
            Arc::clone(&pm.scoring_config), Arc::clone(&pm.callbacks),
        )));
        pm.peers.insert(p2, Arc::new(PeerEntry::with_config(
            peer2, SwarmNodeType::Client, p2,
            Arc::clone(&pm.scoring_config), Arc::clone(&pm.callbacks),
        )));
        pm.peers.insert(p3, Arc::new(PeerEntry::with_config(
            peer3, SwarmNodeType::Client, p3,
            Arc::clone(&pm.scoring_config), Arc::clone(&pm.callbacks),
        )));

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
        let store: Arc<dyn NetPeerStore<StoredPeer>> = Arc::new(MemoryPeerStore::<StoredPeer>::new());

        let pm1 = PeerManager::with_store(
            &mock_identity(), store.clone(), None,
            SwarmScoringConfig::default(), DEFAULT_MAX_PER_BIN,
        );

        for n in 1..=5 {
            pm1.on_peer_ready(test_swarm_peer(n), SwarmNodeType::Storer);
        }
        pm1.ban(&test_overlay(1), Some("bad".to_string()));
        pm1.collect_dirty();
        pm1.flush_write_buffer();

        let pm2 = PeerManager::with_store(
            &mock_identity(), store, None,
            SwarmScoringConfig::default(), DEFAULT_MAX_PER_BIN,
        );
        assert_eq!(pm2.index().len(), 5);
        assert!(pm2.is_banned(&test_overlay(1)));
        assert!(!pm2.is_banned(&test_overlay(2)));
    }

    /// store_discovered_peer preserves handshake-confirmed Storer type.
    #[test]
    fn test_store_discovered_peer_preserves_node_type() {
        let pm = PeerManager::new(&mock_identity());
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        // Peer connected via handshake as Storer
        pm.on_peer_ready(swarm_peer.clone(), SwarmNodeType::Storer);
        assert_eq!(pm.node_type(&overlay), Some(SwarmNodeType::Storer));

        // Gossip re-discovery should NOT overwrite to Client
        pm.store_discovered_peer(swarm_peer);
        assert_eq!(pm.node_type(&overlay), Some(SwarmNodeType::Storer));
    }

    /// store_discovered_peer defaults new peers to Client.
    #[test]
    fn test_store_discovered_peer_defaults_to_client() {
        let pm = PeerManager::new(&mock_identity());
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        pm.store_discovered_peer(swarm_peer);
        assert_eq!(pm.node_type(&overlay), Some(SwarmNodeType::Client));
    }

    /// store_discovered_peers preserves node_type for existing peers.
    #[test]
    fn test_store_discovered_peers_preserves_node_type() {
        let pm = PeerManager::new(&mock_identity());

        // Peers 1 and 2 connected as Storers
        pm.on_peer_ready(test_swarm_peer(1), SwarmNodeType::Storer);
        pm.on_peer_ready(test_swarm_peer(2), SwarmNodeType::Storer);

        // Gossip re-discovers peers 1, 2 (existing) and 3 (new)
        let peers = vec![test_swarm_peer(1), test_swarm_peer(2), test_swarm_peer(3)];
        pm.store_discovered_peers(peers);

        // Existing peers keep Storer type
        assert_eq!(pm.node_type(&test_overlay(1)), Some(SwarmNodeType::Storer));
        assert_eq!(pm.node_type(&test_overlay(2)), Some(SwarmNodeType::Storer));
        // New peer defaults to Client
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

        // Double-ban should not double-count
        pm.ban(&test_overlay(1), None);
        assert_eq!(pm.eligible_count(), 3);
    }

    // --- Hot/cold architecture tests ---

    #[test]
    fn test_gossip_peers_cold() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> = Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = PeerManager::with_store(
            &mock_identity(), store.clone(), None,
            SwarmScoringConfig::default(), DEFAULT_MAX_PER_BIN,
        );

        // Discovered peers should NOT be in DashMap when store is present
        pm.store_discovered_peer(test_swarm_peer(1));
        pm.store_discovered_peer(test_swarm_peer(2));

        assert_eq!(pm.index().len(), 2);
        assert_eq!(pm.peers.len(), 0); // Not in hot cache
    }

    #[test]
    fn test_connected_peers_always_hot() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> = Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = PeerManager::with_store(
            &mock_identity(), store, None,
            SwarmScoringConfig::default(), DEFAULT_MAX_PER_BIN,
        );

        pm.on_peer_ready(test_swarm_peer(1), SwarmNodeType::Client);

        assert_eq!(pm.index().len(), 1);
        assert_eq!(pm.peers.len(), 1); // Connected = always hot
    }

    #[test]
    fn test_get_or_load_promotes() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> = Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = PeerManager::with_store(
            &mock_identity(), store.clone(), None,
            SwarmScoringConfig::default(), DEFAULT_MAX_PER_BIN,
        );

        // Add peer via handshake (hot), then save to store
        pm.on_peer_ready(test_swarm_peer(1), SwarmNodeType::Storer);
        pm.collect_dirty();
        pm.flush_write_buffer();

        // Create fresh PM with same store
        let pm2 = PeerManager::with_store(
            &mock_identity(), store, None,
            SwarmScoringConfig::default(), DEFAULT_MAX_PER_BIN,
        );
        assert_eq!(pm2.peers.len(), 0); // Hot cache empty

        // get_swarm_peer should promote cold→hot
        let peer = pm2.get_swarm_peer(&test_overlay(1));
        assert!(peer.is_some());
        assert_eq!(pm2.peers.len(), 1); // Now in hot cache
    }

    #[test]
    fn test_banned_set_o1() {
        let pm = PeerManager::new(&mock_identity());

        pm.on_peer_ready(test_swarm_peer(1), SwarmNodeType::Client);
        pm.ban(&test_overlay(1), None);

        // Ban check via DashSet (O(1))
        assert!(pm.is_banned(&test_overlay(1)));
        assert!(!pm.is_banned(&test_overlay(2)));
        assert_eq!(pm.banned_count(), 1);
    }

    #[test]
    fn test_write_buffer_flush() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> = Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = PeerManager::with_store(
            &mock_identity(), store.clone(), None,
            SwarmScoringConfig::default(), DEFAULT_MAX_PER_BIN,
        );

        pm.on_peer_ready(test_swarm_peer(1), SwarmNodeType::Client);
        pm.collect_dirty();
        pm.flush_write_buffer();

        // Peer should now be in DB
        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn test_ban_bypasses_buffer() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> = Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = PeerManager::with_store(
            &mock_identity(), store.clone(), None,
            SwarmScoringConfig::default(), DEFAULT_MAX_PER_BIN,
        );

        pm.on_peer_ready(test_swarm_peer(1), SwarmNodeType::Client);
        pm.collect_dirty();
        pm.flush_write_buffer(); // Ensure peer is in DB

        pm.ban(&test_overlay(1), Some("test".into()));

        // Ban should be in DB immediately (no flush needed)
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
        let store: Arc<dyn NetPeerStore<StoredPeer>> = Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = PeerManager::with_store(
            &mock_identity(), store.clone(), None,
            SwarmScoringConfig::default(), DEFAULT_MAX_PER_BIN,
        );

        // Discover (cold) → connect (hot) → save → reload
        pm.store_discovered_peer(test_swarm_peer(1));
        assert_eq!(pm.peers.len(), 0); // Cold

        pm.on_peer_ready(test_swarm_peer(1), SwarmNodeType::Storer);
        assert_eq!(pm.peers.len(), 1); // Hot

        pm.collect_dirty();
        pm.flush_write_buffer();

        // Reload
        let pm2 = PeerManager::with_store(
            &mock_identity(), store, None,
            SwarmScoringConfig::default(), DEFAULT_MAX_PER_BIN,
        );
        assert_eq!(pm2.index().len(), 1);
        assert_eq!(pm2.peers.len(), 0); // Cold after reload

        // Rediscover
        pm2.store_discovered_peer(test_swarm_peer(1));
        assert_eq!(pm2.peers.len(), 0); // Still cold (already known)
    }

    #[test]
    fn test_evict_cold() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> = Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = PeerManager::build(
            &mock_identity(), SwarmScoringConfig::default(), DEFAULT_MAX_PER_BIN,
            Some(store), None, 10, // max_hot_peers = 10
        );

        // Insert 20 peers as "connected" (hot), exceeding max_hot_peers=10
        for n in 1..=20 {
            pm.on_peer_ready(test_swarm_peer(n), SwarmNodeType::Client);
        }
        assert_eq!(pm.peers.len(), 20);

        // Simulate failures on 15 peers (makes them eviction candidates)
        for n in 1..=15 {
            pm.record_dial_failure(&test_overlay(n));
        }

        pm.evict_cold();

        // Should evict down to max_hot_peers (10).
        // 15 candidates with failures, need to evict 10 (20 - 10).
        assert_eq!(pm.peers.len(), 10);

        // The 5 peers without failures (16-20) should still be in hot cache
        for n in 16..=20 {
            assert!(pm.peers.contains_key(&test_overlay(n)));
        }
    }

    #[test]
    fn test_concurrent_hot_cold_access() {
        use std::thread;

        let store: Arc<dyn NetPeerStore<StoredPeer>> = Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = PeerManager::with_store(
            &mock_identity(), store, None,
            SwarmScoringConfig::default(), DEFAULT_MAX_PER_BIN,
        );
        let pm = Arc::new(pm);

        // The PeerManager is already wrapped in Arc from with_store, but our
        // variable shadows it. We need to use the inner Arc.
        // Actually, with_store returns Arc<Self>, so pm is Arc<PeerManager>.
        // We can clone Arc.

        let mut handles = vec![];

        // Writer threads: discover peers
        for batch in 0..4 {
            let pm = Arc::clone(&pm);
            handles.push(thread::spawn(move || {
                for i in 0..25 {
                    pm.store_discovered_peer(test_swarm_peer((batch * 25 + i + 1) as u8));
                }
            }));
        }

        // Reader thread: query peers
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

    // --- Integration tests (Unit 5) ---

    #[test]
    fn test_discover_flush_reload() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> = Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = PeerManager::with_store(
            &mock_identity(), store.clone(), None,
            SwarmScoringConfig::default(), DEFAULT_MAX_PER_BIN,
        );

        // Discover peers via gossip (cold)
        for n in 1..=10 {
            pm.store_discovered_peer(test_swarm_peer(n));
        }
        assert_eq!(pm.index().len(), 10);
        assert_eq!(pm.peers.len(), 0); // All cold

        // Flush write buffer to DB
        pm.flush_write_buffer();
        assert_eq!(store.count().unwrap(), 10);

        // Create new PM with same DB
        let pm2 = PeerManager::with_store(
            &mock_identity(), store, None,
            SwarmScoringConfig::default(), DEFAULT_MAX_PER_BIN,
        );
        assert_eq!(pm2.index().len(), 10);
        assert_eq!(pm2.peers.len(), 0); // All cold

        // All peers should be accessible via get_swarm_peer (cold→hot promotion)
        for n in 1..=10 {
            assert!(pm2.get_swarm_peer(&test_overlay(n)).is_some());
        }
        assert_eq!(pm2.peers.len(), 10); // All promoted to hot
    }

    #[test]
    fn test_full_lifecycle() {
        let db = vertex_storage_redb::RedbDatabase::in_memory().unwrap().into_arc();
        let db_store = Arc::new(crate::db_store::DbPeerStore::new(Arc::clone(&db)));
        db_store.init().unwrap();
        let store: Arc<dyn NetPeerStore<StoredPeer>> = Arc::clone(&db_store) as _;
        let score_store: Option<Arc<dyn SwarmScoreStore<Score = PeerScore, Error = vertex_net_peer_store::StoreError>>> = Some(Arc::clone(&db_store) as _);
        let pm = PeerManager::with_store(
            &mock_identity(), store.clone(), score_store.clone(),
            SwarmScoringConfig::default(), DEFAULT_MAX_PER_BIN,
        );

        // 1. Discover via gossip (cold)
        pm.store_discovered_peer(test_swarm_peer(1));
        assert_eq!(pm.peers.len(), 0);

        // 2. Connect (hot)
        pm.on_peer_ready(test_swarm_peer(1), SwarmNodeType::Storer);
        assert_eq!(pm.peers.len(), 1);

        // 3. Score via success
        {
            let entry = pm.peers.get(&test_overlay(1)).unwrap();
            entry.record_success(Duration::from_millis(50));
            assert!(entry.score() > 0.0);
        }

        // 4. Save to store
        pm.collect_dirty();
        pm.flush_write_buffer();

        // 5. Reload into fresh PM
        let pm2 = PeerManager::with_store(
            &mock_identity(), store, score_store,
            SwarmScoringConfig::default(), DEFAULT_MAX_PER_BIN,
        );
        assert_eq!(pm2.index().len(), 1);
        assert_eq!(pm2.peers.len(), 0); // Cold

        // 6. Verify score survives roundtrip
        let peer = pm2.get_swarm_peer(&test_overlay(1));
        assert!(peer.is_some());
        let entry = pm2.peers.get(&test_overlay(1)).unwrap();
        assert!(entry.score() > 0.0);
        assert_eq!(entry.node_type(), SwarmNodeType::Storer);
    }

    #[test]
    fn test_ban_persistence() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> = Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = PeerManager::with_store(
            &mock_identity(), store.clone(), None,
            SwarmScoringConfig::default(), DEFAULT_MAX_PER_BIN,
        );

        // Connect and ban
        pm.on_peer_ready(test_swarm_peer(1), SwarmNodeType::Client);
        pm.collect_dirty();
        pm.flush_write_buffer();
        pm.ban(&test_overlay(1), Some("test ban".into()));

        // Ban is written directly to DB (bypass buffer)
        let record = store.get(&test_overlay(1)).unwrap().unwrap();
        assert!(record.ban_info.is_some());

        // Reload: ban should persist
        let pm2 = PeerManager::with_store(
            &mock_identity(), store, None,
            SwarmScoringConfig::default(), DEFAULT_MAX_PER_BIN,
        );
        assert!(pm2.is_banned(&test_overlay(1)));
        assert!(!pm2.eligible_peers().contains(&test_overlay(1)));
    }

    #[test]
    fn test_concurrent_gossip_and_queries() {
        use std::thread;

        let store: Arc<dyn NetPeerStore<StoredPeer>> = Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = PeerManager::with_store(
            &mock_identity(), store, None,
            SwarmScoringConfig::default(), DEFAULT_MAX_PER_BIN,
        );
        let pm = Arc::new(pm);

        let mut handles = vec![];

        // Gossip writers
        for batch in 0..4 {
            let pm = Arc::clone(&pm);
            handles.push(thread::spawn(move || {
                for i in 0..25 {
                    let n = (batch * 25 + i + 1) as u8;
                    pm.store_discovered_peer(test_swarm_peer(n));
                }
            }));
        }

        // Query readers: eligible_peers, eligible_count, is_banned
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

        // Flush writer
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
        let store: Arc<dyn NetPeerStore<StoredPeer>> = Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = PeerManager::with_store(
            &mock_identity(), store, None,
            SwarmScoringConfig::default(), DEFAULT_MAX_PER_BIN,
        );

        // Discover many peers via gossip — none should go into DashMap
        for n in 1..=200 {
            pm.store_discovered_peer(test_swarm_peer(n));
        }

        // Index should track all peers
        assert_eq!(pm.index().len(), 200);
        // Hot cache should be empty (all cold when store present)
        assert_eq!(pm.peers.len(), 0);
        // All should be eligible
        assert_eq!(pm.eligible_count(), 200);
    }
}
