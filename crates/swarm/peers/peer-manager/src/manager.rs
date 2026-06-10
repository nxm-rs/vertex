//! Peer manager with hot/cold architecture and database-backed persistence.
//!
//! Connected and recently-accessed peers live in a hot DashMap cache.
//! All known overlays are tracked in the ProximityIndex. Peer data for
//! cold peers lives in the database and is loaded on demand.
//!
//! Capacity bounds and persistence handles are carried by
//! [`PeerManagerConfig`]; see its documentation for how the defaults relate
//! to the topology routing targets and the storage layout.

use std::marker::PhantomData;
use std::sync::Arc;

use dashmap::{DashMap, DashSet};
use metrics::{counter, gauge};
use tokio::sync::broadcast;
use tracing::{debug, warn};
use vertex_net_local::IpCapability;
use vertex_net_peer_registry::ConnectionDirection;
use vertex_net_peer_store::NetPeerStore;
use vertex_net_peer_store::error::StoreError;
use vertex_swarm_api::{
    BanCause, PeerLifecycleEvent, ReportSource, SwarmIdentity, SwarmPeerResolver, SwarmScoreStore,
    SwarmScoringEvent, SwarmSpec,
};
use vertex_swarm_peer::{SwarmPeer, Timestamp, check_timestamp};
use vertex_swarm_peer_score::{PeerScore, SwarmScoringConfig};
use vertex_swarm_primitives::{Bin, OverlayAddress, SwarmNodeType};

use crate::entry::{
    HealthState, PeerEntry, StoredPeer, TrustLevel, on_health_added, on_health_changed,
    on_health_removed, unix_timestamp_secs,
};
use crate::proximity_index::{AddError, ProximityIndex};
use crate::score_distribution::ScoreDistribution;
use crate::write_buffer::WriteBuffer;

/// Cheap-to-clone handle onto the peer manager.
///
/// The manager is always shared behind an `Arc`; cloning the handle is a
/// reference-count bump. The handle is the surface other subsystems hold:
/// it implements [`PeerReporter`](vertex_swarm_api::PeerReporter) (via the
/// trait's `Arc` auto-impl) so protocol handlers, gossip, and topology
/// report scoring events through [`PeerManager::report_peer`], and it
/// exposes [`PeerManager::subscribe`] for the peer lifecycle event stream.
pub type PeerManagerHandle<I> = Arc<PeerManager<I>>;

/// Capacity of the peer lifecycle broadcast channel.
///
/// Sized for connection churn bursts (mass disconnects, trim rounds) so the
/// action-executing subscriber rarely lags. See [`PeerManager::subscribe`]
/// for the lagged-receiver policy.
pub const LIFECYCLE_CHANNEL_CAPACITY: usize = 256;

/// Configuration for [`PeerManager`].
///
/// Carries the scoring policy, the capacity bounds for the in-memory
/// structures, and the optional persistence handles. `Default` yields an
/// ephemeral manager: no store, no score store, every peer held in memory.
///
/// # Capacity defaults
///
/// The capacity bounds size the in-memory structures; they are not specified
/// by the Book of Swarm and exist relative to each other:
///
/// - [`Self::DEFAULT_MAX_PER_BIN`] (128) bounds how many overlays the
///   proximity index keeps per bin. Topology targets 3-35 connected peers per
///   bin, so 128 leaves 3.7-42x headroom for the known-but-unconnected
///   candidates a bin draws dials from when it is below target.
/// - [`Self::DEFAULT_MAX_HOT_PEERS`] (500) bounds the hot DashMap cache of
///   full peer records before eviction to cold storage. It comfortably covers
///   the connected set (total routing target ~160 plus inbound headroom) and
///   the recently-touched cold peers a dial round visits, so a steady-state
///   node serves from memory while colder peers spill to the database.
/// - [`Self::DEFAULT_WRITE_BUFFER_CAPACITY`] (64) is the number of dirty
///   records buffered before an automatic flush. A buffered record is a few
///   hundred bytes (overlay, multiaddrs, handshake signature, nonce,
///   timestamps), so a full flush writes on the order of 10-20KB in one batch,
///   amortizing store writes without holding enough in memory to lose much on
///   a crash.
#[derive(Clone)]
pub struct PeerManagerConfig {
    /// Peer scoring weights and ban/warn thresholds.
    pub scoring: SwarmScoringConfig,
    /// Maximum overlays tracked per proximity bin in the index.
    pub max_per_bin: usize,
    /// Maximum peers in the hot cache before eviction to cold storage.
    pub max_hot_peers: usize,
    /// Number of dirty records buffered before an automatic flush.
    pub write_buffer_capacity: usize,
    /// Cold-storage backend; `None` keeps every peer in memory.
    pub store: Option<Arc<dyn NetPeerStore<StoredPeer>>>,
    /// Score persistence; `None` keeps scores in memory only.
    pub score_store: Option<Arc<dyn SwarmScoreStore<Score = PeerScore, Error = StoreError>>>,
}

impl PeerManagerConfig {
    /// Default maximum overlays per proximity bin in the index.
    ///
    /// With topology routing targets of 3-35 connected peers per bin, 128
    /// gives 3.7-42x headroom for unconnected dial candidates.
    pub const DEFAULT_MAX_PER_BIN: usize = 128;

    /// Default maximum hot peers in the DashMap cache.
    ///
    /// Covers the connected set plus recently-touched cold peers; see the
    /// capacity notes on [`PeerManagerConfig`].
    pub const DEFAULT_MAX_HOT_PEERS: usize = 500;

    /// Default number of dirty records buffered before an automatic flush.
    ///
    /// Amortizes store writes; see the capacity notes on
    /// [`PeerManagerConfig`].
    pub const DEFAULT_WRITE_BUFFER_CAPACITY: usize = 64;
}

impl Default for PeerManagerConfig {
    fn default() -> Self {
        Self {
            scoring: SwarmScoringConfig::default(),
            max_per_bin: Self::DEFAULT_MAX_PER_BIN,
            max_hot_peers: Self::DEFAULT_MAX_HOT_PEERS,
            write_buffer_capacity: Self::DEFAULT_WRITE_BUFFER_CAPACITY,
            store: None,
            score_store: None,
        }
    }
}

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
    pub(crate) score_store: Option<Arc<dyn SwarmScoreStore<Score = PeerScore, Error = StoreError>>>,
    /// O(1) ban checks without DB or DashMap lookup.
    pub(crate) banned_set: DashSet<OverlayAddress>,
    /// Batches DB writes for amortized flush.
    pub(crate) write_buffer: WriteBuffer,
    /// Scoring configuration.
    pub(crate) scoring_config: Arc<SwarmScoringConfig>,
    /// Maximum peers in hot DashMap cache before eviction.
    pub(crate) max_hot_peers: usize,
    /// Per-bucket gauge tracking of score distribution.
    pub(crate) score_distribution: Arc<ScoreDistribution>,
    /// Peer lifecycle event broadcast (see [`Self::subscribe`]).
    pub(crate) lifecycle_tx: broadcast::Sender<PeerLifecycleEvent>,
}

impl<I: SwarmIdentity> PeerManager<I> {
    /// Create a peer manager for `identity` from `config`.
    ///
    /// With `config.store` set, the overlay index and banned set are loaded
    /// from the store on construction; the hot cache starts empty, peers are
    /// promoted on access, and scores are loaded lazily from
    /// `config.score_store` during promotion. Without a store, all peers live
    /// in the hot cache.
    pub fn new(identity: &I, config: PeerManagerConfig) -> Arc<Self> {
        let PeerManagerConfig {
            scoring,
            max_per_bin,
            max_hot_peers,
            write_buffer_capacity,
            store,
            score_store,
        } = config;
        let local_overlay = identity.overlay_address();
        let max_po = identity.spec().max_po();
        let (lifecycle_tx, _) = broadcast::channel(LIFECYCLE_CHANNEL_CAPACITY);
        let pm = Arc::new(Self {
            _identity: PhantomData,
            index: ProximityIndex::new(local_overlay, max_po, max_per_bin),
            peers: DashMap::new(),
            store,
            score_store,
            banned_set: DashSet::new(),
            write_buffer: WriteBuffer::new(write_buffer_capacity),
            scoring_config: Arc::new(scoring),
            max_hot_peers,
            score_distribution: Arc::new(ScoreDistribution::new()),
            lifecycle_tx,
        });
        if pm.store.is_some() {
            pm.load_index_from_store();
        }
        pm
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
    pub fn storer_overlays_in_bin(&self, bin: Bin, count: usize) -> Vec<OverlayAddress> {
        let mut result = Vec::new();
        let mut cold_candidates = Vec::new();

        self.index.filter_bin(bin, count + count, |overlay| {
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
    pub fn dialable_overlays_in_bin(&self, bin: Bin, count: usize) -> Vec<OverlayAddress> {
        let mut result = Vec::new();
        let mut cold_candidates = Vec::new();

        self.index.filter_bin(bin, count + count, |overlay| {
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
    pub fn dialable_in_bin(&self, bin: Bin, count: usize) -> Vec<SwarmPeer> {
        let overlays = self.dialable_overlays_in_bin(bin, count);
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

    /// Ban a peer (prevents dialing) and emit [`PeerLifecycleEvent::Banned`].
    ///
    /// Topology subscribes to the lifecycle stream and closes the banned
    /// peer's connection. Bans are written directly to the DB (bypassing the
    /// write buffer) for immediacy. Bans currently have no scheduled expiry.
    pub fn ban(&self, overlay: &OverlayAddress, cause: BanCause, reason: Option<String>) {
        if !self.banned_set.insert(*overlay) {
            return; // Already banned
        }

        gauge!("peer_manager_banned_peers").increment(1.0);

        // Update hot peer entry if present
        if let Some(entry) = self.peers.get(overlay) {
            if !entry.is_banned() {
                let old_state = entry.health_state();
                warn!(?overlay, %cause, ?reason, "banning peer");
                entry.ban(reason.clone());
                on_health_changed(old_state, HealthState::Banned);
            }
        } else {
            warn!(?overlay, %cause, ?reason, "banning cold peer");
        }

        // Write ban directly to DB (bypass buffer for immediacy)
        if let Some(ref store) = self.store
            && let Ok(Some(mut record)) = store.get(overlay)
        {
            record.ban_info = Some((unix_timestamp_secs(), reason.unwrap_or_default()));
            let _ = store.save(&record);
        }

        self.emit(PeerLifecycleEvent::Banned {
            overlay: *overlay,
            until: None,
            reason: cause,
        });
    }

    /// Subscribe to the peer lifecycle event stream.
    ///
    /// The stream carries every [`PeerLifecycleEvent`]: connects,
    /// disconnects, score warnings, disconnect requests, and bans.
    ///
    /// # Lagged-receiver policy
    ///
    /// The channel holds [`LIFECYCLE_CHANNEL_CAPACITY`] events; a receiver
    /// that falls further behind observes `RecvError::Lagged` and loses the
    /// oldest events. Observability subscribers simply tolerate the gap. The
    /// one action-executing subscriber (topology, which closes connections
    /// for `DisconnectRequested` and `Banned`) must treat `Lagged` as a
    /// resynchronization point: it re-reads the banned set (via
    /// [`Self::is_banned`]) and closes
    /// any still-connected banned peer, so a lagged stream can never strand
    /// a banned peer connected. A `DisconnectRequested` lost to lag is not
    /// replayed; the peer stays connected until its score next crosses a
    /// threshold (the ban threshold is level-triggered, so continued abuse
    /// escalates to a ban that is reconciled exactly).
    pub fn subscribe(&self) -> broadcast::Receiver<PeerLifecycleEvent> {
        self.lifecycle_tx.subscribe()
    }

    /// Broadcast a lifecycle event, ignoring the no-subscriber case.
    pub(crate) fn emit(&self, event: PeerLifecycleEvent) {
        let _ = self.lifecycle_tx.send(event);
    }

    /// Store a single discovered peer.
    ///
    /// Discovered peers arrive via hive gossip, so the gossip timestamp policy
    /// (via [`check_timestamp`]) is applied before
    /// any overwrite: a record that is stale, replayed, too frequent, or dated
    /// implausibly far into the future is dropped rather than clobbering the
    /// stored record. Rejections increment `peer_manager_gossip_timestamp_rejected_total`
    /// with a `reason` label and the existing overlay is returned unchanged.
    ///
    /// For hot peers, updates addresses (preserves handshake-confirmed node_type).
    /// For cold peers, just touches the LRU index.
    /// For new peers, adds to index and buffers a write to DB.
    /// Without a store, inserts directly into DashMap.
    pub fn store_discovered_peer(&self, swarm_peer: SwarmPeer) -> OverlayAddress {
        let overlay = OverlayAddress::from(*swarm_peer.overlay());
        if let Some(entry) = self.peers.get(&overlay) {
            // Hot peer - resolve the gossip timestamp against the stored record
            // before overwriting, then update addresses (dirty flag set by entry).
            if self.reject_stale_gossip(&overlay, swarm_peer.timestamp(), Some(entry.timestamp())) {
                return overlay;
            }
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
                    // Bin at capacity - save to DB only, don't add to index.
                    // Resolve against the persisted record's timestamp first so
                    // a stale gossip record cannot overwrite a newer stored one.
                    let existing = self
                        .store
                        .as_ref()
                        .and_then(|s| s.get(&overlay).ok().flatten());
                    if self.reject_stale_gossip(
                        &overlay,
                        swarm_peer.timestamp(),
                        existing.as_ref().map(|r| r.peer.timestamp()),
                    ) {
                        return overlay;
                    }
                    let mut record = StoredPeer::new_discovered(swarm_peer);
                    if let Some(existing) = existing {
                        // Gossip refreshes addresses; it never changes a
                        // previously stored node type.
                        record.node_type = existing.node_type;
                    }
                    if self.write_buffer.push(record) {
                        self.flush_write_buffer();
                    }
                }
            }
        } else {
            // No store - insert into DashMap (backward compat). The
            // provisional refresh is dropped if a concurrent handshake has
            // already confirmed the node type.
            let entry = self.insert_peer(overlay, swarm_peer, SwarmNodeType::Client);
            entry.set_provisional_node_type(SwarmNodeType::Client);
        }
        overlay
    }

    /// Apply the gossip timestamp policy for a known peer.
    ///
    /// Returns `true` when the candidate record must be dropped (the caller
    /// keeps the stored record untouched). On rejection a
    /// `peer_manager_gossip_timestamp_rejected_total` counter is incremented
    /// with the rejection `reason` label and a debug line is logged.
    fn reject_stale_gossip(
        &self,
        overlay: &OverlayAddress,
        candidate: Timestamp,
        existing: Option<Timestamp>,
    ) -> bool {
        let now = Timestamp::from_seconds(unix_timestamp_secs() as i64);
        match check_timestamp(candidate, existing, now) {
            Ok(()) => false,
            Err(rejection) => {
                counter!(
                    "peer_manager_gossip_timestamp_rejected_total",
                    "reason" => rejection.reason(),
                )
                .increment(1);
                debug!(
                    ?overlay,
                    reason = rejection.reason(),
                    candidate = candidate.get(),
                    existing = existing.map(|t| t.get()),
                    "dropping stale gossip peer record"
                );
                true
            }
        }
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

    /// Called by topology when a peer completes the handshake.
    ///
    /// Always inserts into the hot cache. Confirms the handshake-asserted
    /// node type (from here on gossip cannot change it; only a later
    /// handshake may re-confirm a different value), records the connection
    /// state (connected-since, direction) and the topology-computed
    /// [`TrustLevel`] on the entry, emits [`PeerLifecycleEvent::Connected`],
    /// and reports the connection success through [`Self::report_peer`].
    pub fn on_peer_connected(
        &self,
        swarm_peer: SwarmPeer,
        node_type: SwarmNodeType,
        direction: ConnectionDirection,
        trust: TrustLevel,
    ) {
        let overlay = OverlayAddress::from(*swarm_peer.overlay());
        debug!(?overlay, ?node_type, %direction, %trust, "peer connected");

        let entry = self.insert_peer(overlay, swarm_peer, node_type);
        entry.confirm_node_type(node_type);
        let old_state = entry.health_state();
        entry.set_connected(direction, trust);
        on_health_changed(old_state, entry.health_state());

        self.emit(PeerLifecycleEvent::Connected { overlay, node_type });
        self.report_peer(
            &overlay,
            SwarmScoringEvent::ConnectionSuccess { latency: None },
            ReportSource::Topology,
        );
    }

    /// Called by topology when the last connection to a peer closes.
    ///
    /// Clears the connection state recorded by [`Self::on_peer_connected`]
    /// and emits [`PeerLifecycleEvent::Disconnected`]. `reason` is a static
    /// label for the debug log; scoring consequences (early-disconnect
    /// penalties) go through [`Self::record_early_disconnect`].
    pub fn on_peer_disconnected(&self, overlay: &OverlayAddress, reason: &'static str) {
        debug!(?overlay, reason, "peer disconnected");
        if let Some(entry) = self.peers.get(overlay) {
            entry.clear_connected();
        }
        self.emit(PeerLifecycleEvent::Disconnected { overlay: *overlay });
    }

    /// Stored [`TrustLevel`] for a peer (one atomic load on the hot entry).
    ///
    /// Defaults to [`TrustLevel::Normal`] for cold or unknown peers; the
    /// level is process-local and recomputed at every handshake.
    #[must_use]
    pub fn trust_level(&self, overlay: &OverlayAddress) -> TrustLevel {
        self.peers
            .get(overlay)
            .map(|e| e.trust_level())
            .unwrap_or_default()
    }

    /// Whether the peer currently has a handshake-complete connection.
    #[must_use]
    pub fn is_connected(&self, overlay: &OverlayAddress) -> bool {
        self.peers.get(overlay).is_some_and(|e| e.is_connected())
    }

    /// Unix seconds at which the peer's current connection completed its
    /// handshake, or `None` while disconnected.
    #[must_use]
    pub fn connected_since(&self, overlay: &OverlayAddress) -> Option<u64> {
        self.peers.get(overlay).and_then(|e| e.connected_since())
    }

    /// Direction of the peer's current connection, or `None` while
    /// disconnected.
    #[must_use]
    pub fn connection_direction(&self, overlay: &OverlayAddress) -> Option<ConnectionDirection> {
        self.peers.get(overlay).and_then(|e| e.direction())
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
            .and_then(|ss| ss.get_score(overlay).ok().flatten());

        use dashmap::mapref::entry::Entry;
        match self.peers.entry(*overlay) {
            Entry::Occupied(e) => Some(Arc::clone(e.get())),
            Entry::Vacant(e) => {
                let entry = Arc::new(PeerEntry::from_record(
                    record,
                    score,
                    Arc::clone(&self.scoring_config),
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

    /// Insert or update a peer in the hot cache, returning the entry.
    ///
    /// `node_type` only seeds new entries (as a provisional value); existing
    /// entries get their addresses refreshed and keep their node type.
    /// Callers apply the source-appropriate node type write on the returned
    /// entry: `confirm_node_type` for handshakes, `set_provisional_node_type`
    /// for gossip.
    fn insert_peer(
        &self,
        overlay: OverlayAddress,
        peer: SwarmPeer,
        node_type: SwarmNodeType,
    ) -> Arc<PeerEntry> {
        use dashmap::mapref::entry::Entry;

        match self.peers.entry(overlay) {
            Entry::Occupied(e) => {
                e.get().update_addresses(peer);
                self.index.touch(&overlay);
                Arc::clone(e.get())
            }
            Entry::Vacant(e) => {
                let entry = Arc::new(PeerEntry::with_config(
                    peer,
                    node_type,
                    overlay,
                    Arc::clone(&self.scoring_config),
                ));
                let initial_score = entry.score();
                let cloned = Arc::clone(&entry);
                e.insert(entry);
                if self.index.add(overlay).is_ok() {
                    gauge!("peer_manager_total_peers").increment(1.0);
                }
                gauge!("peer_manager_hot_peers").set(self.peers.len() as f64);
                self.score_distribution.on_peer_added(initial_score);
                on_health_added(HealthState::Healthy);
                cloned
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
    use std::time::Duration;
    use vertex_net_peer_store::MemoryPeerStore;
    use vertex_swarm_api::DisconnectCause;
    use vertex_swarm_api::SwarmScoreStore;
    use vertex_swarm_test_utils::{
        MockIdentity, test_overlay, test_swarm_peer, test_swarm_peer_with_timestamp,
    };

    fn mock_identity() -> MockIdentity {
        MockIdentity::with_overlay(test_overlay(0))
    }

    /// Ephemeral manager with default config.
    fn manager() -> Arc<PeerManager<MockIdentity>> {
        PeerManager::new(&mock_identity(), PeerManagerConfig::default())
    }

    /// Manager backed by `store`, otherwise default config.
    fn manager_with_store(
        store: Arc<dyn NetPeerStore<StoredPeer>>,
        score_store: Option<Arc<dyn SwarmScoreStore<Score = PeerScore, Error = StoreError>>>,
    ) -> Arc<PeerManager<MockIdentity>> {
        PeerManager::new(
            &mock_identity(),
            PeerManagerConfig {
                store: Some(store),
                score_store,
                ..Default::default()
            },
        )
    }

    #[test]
    fn test_store_discovered_peer() {
        let pm = manager();
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        let stored = pm.store_discovered_peer(swarm_peer.clone());
        assert_eq!(stored, overlay);
        assert!(pm.get_swarm_peer(&overlay).is_some());
        assert!(pm.index().exists(&overlay));
    }

    #[test]
    fn test_store_discovered_peer_rejects_older_timestamp() {
        let pm = manager();
        let overlay = test_overlay(1);
        let base = 1_700_000_000;

        // Seed a known peer with a recent record (port 1000).
        let newer = test_swarm_peer_with_timestamp(1, base, 1000);
        pm.store_discovered_peer(newer);
        assert!(
            pm.get_swarm_peer(&overlay)
                .unwrap()
                .multiaddr()
                .unwrap()
                .to_string()
                .contains("/tcp/1000/")
        );

        // An older gossip record (port 2000) must NOT overwrite the stored one.
        let older = test_swarm_peer_with_timestamp(1, base - 10_000, 2000);
        let stored = pm.store_discovered_peer(older);
        assert_eq!(stored, overlay);
        assert!(
            pm.get_swarm_peer(&overlay)
                .unwrap()
                .multiaddr()
                .unwrap()
                .to_string()
                .contains("/tcp/1000/"),
            "older gossip record must not clobber the newer stored addresses"
        );
    }

    #[test]
    fn test_store_discovered_peer_accepts_sufficiently_newer_timestamp() {
        let pm = manager();
        let overlay = test_overlay(1);
        let base = 1_700_000_000;

        // Seed with an old record (port 1000).
        pm.store_discovered_peer(test_swarm_peer_with_timestamp(1, base, 1000));

        // A record well beyond MIN_UPDATE_INTERVAL (port 3000) overwrites it.
        let interval = vertex_swarm_peer::MIN_UPDATE_INTERVAL.as_secs() as i64;
        let fresh = test_swarm_peer_with_timestamp(1, base + interval + 1, 3000);
        pm.store_discovered_peer(fresh);
        assert!(
            pm.get_swarm_peer(&overlay)
                .unwrap()
                .multiaddr()
                .unwrap()
                .to_string()
                .contains("/tcp/3000/"),
            "a sufficiently newer record must replace the stored addresses"
        );
    }

    #[test]
    fn test_store_discovered_peer_rejects_too_soon_timestamp() {
        let pm = manager();
        let overlay = test_overlay(1);
        let base = 1_700_000_000;

        pm.store_discovered_peer(test_swarm_peer_with_timestamp(1, base, 1000));

        // Newer, but inside MIN_UPDATE_INTERVAL: dropped as too_soon.
        let too_soon = test_swarm_peer_with_timestamp(1, base + 10, 4000);
        pm.store_discovered_peer(too_soon);
        assert!(
            pm.get_swarm_peer(&overlay)
                .unwrap()
                .multiaddr()
                .unwrap()
                .to_string()
                .contains("/tcp/1000/"),
            "a too-soon gossip record must not replace the stored addresses"
        );
    }

    #[test]
    fn test_on_peer_connected() {
        let pm = manager();
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        pm.on_peer_connected(
            swarm_peer,
            SwarmNodeType::Storer,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );
        assert_eq!(pm.node_type(&overlay), Some(SwarmNodeType::Storer));
        assert!(pm.index().exists(&overlay));
    }

    #[test]
    fn test_peer_lifecycle() {
        let pm = manager();
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        pm.store_discovered_peer(swarm_peer.clone());
        assert!(pm.eligible_peers().contains(&overlay));

        pm.on_peer_connected(
            swarm_peer,
            SwarmNodeType::Client,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );
        assert!(pm.eligible_peers().contains(&overlay));
    }

    #[test]
    fn test_ban() {
        let pm = manager();
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        pm.on_peer_connected(
            swarm_peer,
            SwarmNodeType::Client,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );
        pm.ban(
            &overlay,
            BanCause::Requested,
            Some("misbehaving".to_string()),
        );

        assert!(pm.is_banned(&overlay));
        assert!(!pm.eligible_peers().contains(&overlay));
    }

    #[test]
    fn test_get_dialable_peers() {
        let pm = manager();

        for n in 1..=5 {
            pm.store_discovered_peer(test_swarm_peer(n));
        }

        pm.ban(&test_overlay(1), BanCause::Requested, None);

        let all_overlays: Vec<_> = (1..=5).map(test_overlay).collect();
        let dialable = pm.get_dialable_peers(&all_overlays);

        assert_eq!(dialable.len(), 4);
    }

    #[test]
    fn test_custom_scoring_config() {
        let config = SwarmScoringConfig::lenient();
        let pm = PeerManager::new(
            &mock_identity(),
            PeerManagerConfig {
                scoring: config,
                ..Default::default()
            },
        );

        assert!(pm.scoring_config.ban_threshold() < 0.0);
    }

    #[test]
    fn test_known_storer_overlays() {
        let pm = manager();

        pm.on_peer_connected(
            test_swarm_peer(1),
            SwarmNodeType::Storer,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );
        pm.on_peer_connected(
            test_swarm_peer(2),
            SwarmNodeType::Client,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );
        pm.on_peer_connected(
            test_swarm_peer(3),
            SwarmNodeType::Storer,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );
        pm.on_peer_connected(
            test_swarm_peer(4),
            SwarmNodeType::Client,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );

        pm.ban(&test_overlay(1), BanCause::Requested, None);

        let storers = pm.known_storer_overlays();
        assert_eq!(storers.len(), 1);
        assert!(storers.contains(&test_overlay(3)));
    }

    #[test]
    fn test_get_swarm_peers() {
        let pm = manager();

        for n in 1..=5 {
            pm.on_peer_connected(
                test_swarm_peer(n),
                SwarmNodeType::Storer,
                ConnectionDirection::Outbound,
                TrustLevel::Normal,
            );
        }

        let overlays = vec![test_overlay(1), test_overlay(3), test_overlay(5)];
        let peers = pm.get_swarm_peers(&overlays);

        assert_eq!(peers.len(), 3);
    }

    #[test]
    fn test_get_swarm_peers_missing() {
        let pm = manager();

        pm.on_peer_connected(
            test_swarm_peer(1),
            SwarmNodeType::Storer,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );

        let overlays = vec![test_overlay(1), test_overlay(99)];
        let peers = pm.get_swarm_peers(&overlays);

        assert_eq!(peers.len(), 1);
    }

    #[test]
    fn test_node_type_variants() {
        let pm = manager();

        pm.on_peer_connected(
            test_swarm_peer(1),
            SwarmNodeType::Bootnode,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );
        pm.on_peer_connected(
            test_swarm_peer(2),
            SwarmNodeType::Client,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );
        pm.on_peer_connected(
            test_swarm_peer(3),
            SwarmNodeType::Storer,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );

        assert_eq!(
            pm.node_type(&test_overlay(1)),
            Some(SwarmNodeType::Bootnode)
        );
        assert_eq!(pm.node_type(&test_overlay(2)), Some(SwarmNodeType::Client));
        assert_eq!(pm.node_type(&test_overlay(3)), Some(SwarmNodeType::Storer));
    }

    #[test]
    fn test_bin_index_integration() {
        let pm = manager();

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
        let pm = manager();

        pm.store_discovered_peer(test_swarm_peer(1));
        pm.store_discovered_peer(test_swarm_peer(2));
        pm.store_discovered_peer(test_swarm_peer(3));

        pm.store_discovered_peer(test_swarm_peer(1));
    }

    #[test]
    fn test_dialable_in_bin() {
        let pm = manager();

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
            )),
        );
        pm.peers.insert(
            p2,
            Arc::new(PeerEntry::with_config(
                peer2,
                SwarmNodeType::Client,
                p2,
                Arc::clone(&pm.scoring_config),
            )),
        );
        pm.peers.insert(
            p3,
            Arc::new(PeerEntry::with_config(
                peer3,
                SwarmNodeType::Client,
                p3,
                Arc::clone(&pm.scoring_config),
            )),
        );

        pm.ban(&p1, BanCause::Requested, None);

        let dialable = pm.dialable_in_bin(Bin::new(0).unwrap(), 2);
        assert_eq!(dialable.len(), 2);
    }

    #[test]
    fn test_get_swarm_peer() {
        let pm = manager();
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

        let pm1 = manager_with_store(store.clone(), None);

        for n in 1..=5 {
            pm1.on_peer_connected(
                test_swarm_peer(n),
                SwarmNodeType::Storer,
                ConnectionDirection::Outbound,
                TrustLevel::Normal,
            );
        }
        pm1.ban(
            &test_overlay(1),
            BanCause::Requested,
            Some("bad".to_string()),
        );
        pm1.collect_dirty();
        pm1.flush_write_buffer();

        let pm2 = manager_with_store(store, None);
        assert_eq!(pm2.index().len(), 5);
        assert!(pm2.is_banned(&test_overlay(1)));
        assert!(!pm2.is_banned(&test_overlay(2)));
    }

    #[test]
    fn test_store_discovered_peer_preserves_node_type() {
        let pm = manager();
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        pm.on_peer_connected(
            swarm_peer.clone(),
            SwarmNodeType::Storer,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );
        assert_eq!(pm.node_type(&overlay), Some(SwarmNodeType::Storer));

        pm.store_discovered_peer(swarm_peer);
        assert_eq!(pm.node_type(&overlay), Some(SwarmNodeType::Storer));
    }

    #[test]
    fn test_reconnect_handshake_reconfirms_node_type() {
        let pm = PeerManager::new(&mock_identity(), PeerManagerConfig::default());
        let overlay = test_overlay(1);

        pm.on_peer_connected(
            test_swarm_peer(1),
            SwarmNodeType::Client,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );
        assert_eq!(pm.node_type(&overlay), Some(SwarmNodeType::Client));

        // Gossip cannot change the confirmed type.
        pm.store_discovered_peer(test_swarm_peer(1));
        assert_eq!(pm.node_type(&overlay), Some(SwarmNodeType::Client));

        // The node upgraded between sessions; the new handshake re-confirms.
        pm.on_peer_connected(
            test_swarm_peer(1),
            SwarmNodeType::Storer,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );
        assert_eq!(pm.node_type(&overlay), Some(SwarmNodeType::Storer));

        // Gossip still cannot change it afterwards.
        pm.store_discovered_peer(test_swarm_peer(1));
        assert_eq!(pm.node_type(&overlay), Some(SwarmNodeType::Storer));
    }

    #[test]
    fn test_store_discovered_peer_defaults_to_client() {
        let pm = manager();
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        pm.store_discovered_peer(swarm_peer);
        assert_eq!(pm.node_type(&overlay), Some(SwarmNodeType::Client));
    }

    #[test]
    fn test_store_discovered_peers_preserves_node_type() {
        let pm = manager();

        pm.on_peer_connected(
            test_swarm_peer(1),
            SwarmNodeType::Storer,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );
        pm.on_peer_connected(
            test_swarm_peer(2),
            SwarmNodeType::Storer,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );

        let peers = vec![test_swarm_peer(1), test_swarm_peer(2), test_swarm_peer(3)];
        pm.store_discovered_peers(peers);

        assert_eq!(pm.node_type(&test_overlay(1)), Some(SwarmNodeType::Storer));
        assert_eq!(pm.node_type(&test_overlay(2)), Some(SwarmNodeType::Storer));
        assert_eq!(pm.node_type(&test_overlay(3)), Some(SwarmNodeType::Client));
    }

    #[test]
    fn test_banned_count_tracking() {
        let pm = manager();

        for n in 1..=5 {
            pm.store_discovered_peer(test_swarm_peer(n));
        }

        assert_eq!(pm.eligible_count(), 5);
        pm.ban(&test_overlay(1), BanCause::Requested, None);
        assert_eq!(pm.eligible_count(), 4);
        pm.ban(&test_overlay(2), BanCause::Requested, None);
        assert_eq!(pm.eligible_count(), 3);

        pm.ban(&test_overlay(1), BanCause::Requested, None);
        assert_eq!(pm.eligible_count(), 3);
    }

    #[test]
    fn test_gossip_peers_cold() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> =
            Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = manager_with_store(store.clone(), None);

        pm.store_discovered_peer(test_swarm_peer(1));
        pm.store_discovered_peer(test_swarm_peer(2));

        assert_eq!(pm.index().len(), 2);
        assert_eq!(pm.peers.len(), 0);
    }

    #[test]
    fn test_connected_peers_always_hot() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> =
            Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = manager_with_store(store, None);

        pm.on_peer_connected(
            test_swarm_peer(1),
            SwarmNodeType::Client,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );

        assert_eq!(pm.index().len(), 1);
        assert_eq!(pm.peers.len(), 1);
    }

    #[test]
    fn test_get_or_load_promotes() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> =
            Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = manager_with_store(store.clone(), None);

        pm.on_peer_connected(
            test_swarm_peer(1),
            SwarmNodeType::Storer,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );
        pm.collect_dirty();
        pm.flush_write_buffer();

        let pm2 = manager_with_store(store, None);
        assert_eq!(pm2.peers.len(), 0);

        let peer = pm2.get_swarm_peer(&test_overlay(1));
        assert!(peer.is_some());
        assert_eq!(pm2.peers.len(), 1);
    }

    #[test]
    fn test_banned_set_o1() {
        let pm = manager();

        pm.on_peer_connected(
            test_swarm_peer(1),
            SwarmNodeType::Client,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );
        pm.ban(&test_overlay(1), BanCause::Requested, None);

        assert!(pm.is_banned(&test_overlay(1)));
        assert!(!pm.is_banned(&test_overlay(2)));
        assert_eq!(pm.banned_count(), 1);
    }

    #[test]
    fn test_write_buffer_flush() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> =
            Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = manager_with_store(store.clone(), None);

        pm.on_peer_connected(
            test_swarm_peer(1),
            SwarmNodeType::Client,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );
        pm.collect_dirty();
        pm.flush_write_buffer();

        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn test_ban_bypasses_buffer() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> =
            Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = manager_with_store(store.clone(), None);

        pm.on_peer_connected(
            test_swarm_peer(1),
            SwarmNodeType::Client,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );
        pm.collect_dirty();
        pm.flush_write_buffer();

        pm.ban(&test_overlay(1), BanCause::Requested, Some("test".into()));

        let record = store.get(&test_overlay(1)).unwrap().unwrap();
        assert!(record.ban_info.is_some());
    }

    #[test]
    fn test_eligible_count_o1() {
        let pm = manager();

        for n in 1..=10 {
            pm.store_discovered_peer(test_swarm_peer(n));
        }

        assert_eq!(pm.eligible_count(), 10);
        pm.ban(&test_overlay(1), BanCause::Requested, None);
        pm.ban(&test_overlay(2), BanCause::Requested, None);
        assert_eq!(pm.eligible_count(), 8);
    }

    #[test]
    fn test_db_roundtrip_hot_cold() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> =
            Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = manager_with_store(store.clone(), None);

        pm.store_discovered_peer(test_swarm_peer(1));
        assert_eq!(pm.peers.len(), 0);

        pm.on_peer_connected(
            test_swarm_peer(1),
            SwarmNodeType::Storer,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );
        assert_eq!(pm.peers.len(), 1);

        pm.collect_dirty();
        pm.flush_write_buffer();

        let pm2 = manager_with_store(store, None);
        assert_eq!(pm2.index().len(), 1);
        assert_eq!(pm2.peers.len(), 0);

        pm2.store_discovered_peer(test_swarm_peer(1));
        assert_eq!(pm2.peers.len(), 0);
    }

    #[test]
    fn test_evict_cold() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> =
            Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = PeerManager::new(
            &mock_identity(),
            PeerManagerConfig {
                store: Some(store),
                max_hot_peers: 10,
                ..Default::default()
            },
        );

        for n in 1..=20 {
            pm.on_peer_connected(
                test_swarm_peer(n),
                SwarmNodeType::Client,
                ConnectionDirection::Outbound,
                TrustLevel::Normal,
            );
        }
        assert_eq!(pm.peers.len(), 20);

        // Disconnect and fail the first fifteen; connected peers are never
        // eviction candidates regardless of their failure count.
        for n in 1..=15 {
            pm.on_peer_disconnected(&test_overlay(n), "test");
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
        let pm = manager_with_store(store, None);
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
        let pm = manager_with_store(store.clone(), None);

        for n in 1..=10 {
            pm.store_discovered_peer(test_swarm_peer(n));
        }
        assert_eq!(pm.index().len(), 10);
        assert_eq!(pm.peers.len(), 0);

        pm.flush_write_buffer();
        assert_eq!(store.count().unwrap(), 10);

        let pm2 = manager_with_store(store, None);
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
                        Score = PeerScore,
                        Error = vertex_net_peer_store::error::StoreError,
                    >,
            >,
        > = Some(Arc::clone(&db_store) as _);
        let pm = manager_with_store(store.clone(), score_store.clone());

        pm.store_discovered_peer(test_swarm_peer(1));
        assert_eq!(pm.peers.len(), 0);

        pm.on_peer_connected(
            test_swarm_peer(1),
            SwarmNodeType::Storer,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );
        assert_eq!(pm.peers.len(), 1);

        {
            let entry = pm.peers.get(&test_overlay(1)).unwrap();
            entry.record_event(SwarmScoringEvent::ConnectionSuccess {
                latency: Some(Duration::from_millis(50)),
            });
            assert!(entry.score() > 0.0);
        }

        pm.collect_dirty();
        pm.flush_write_buffer();

        let pm2 = manager_with_store(store, score_store);
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
        let pm = manager_with_store(store.clone(), None);

        pm.on_peer_connected(
            test_swarm_peer(1),
            SwarmNodeType::Client,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );
        pm.collect_dirty();
        pm.flush_write_buffer();
        pm.ban(
            &test_overlay(1),
            BanCause::Requested,
            Some("test ban".into()),
        );

        let record = store.get(&test_overlay(1)).unwrap().unwrap();
        assert!(record.ban_info.is_some());

        let pm2 = manager_with_store(store, None);
        assert!(pm2.is_banned(&test_overlay(1)));
        assert!(!pm2.eligible_peers().contains(&test_overlay(1)));
    }

    #[test]
    fn test_concurrent_gossip_and_queries() {
        use std::thread;

        let store: Arc<dyn NetPeerStore<StoredPeer>> =
            Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = manager_with_store(store, None);
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

    fn drain_events(rx: &mut broadcast::Receiver<PeerLifecycleEvent>) -> Vec<PeerLifecycleEvent> {
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        events
    }

    /// Manager with tight thresholds so a handful of reports walks the score
    /// through warn (-10), disconnect (-20), and ban (-30).
    fn thresholds_manager() -> Arc<PeerManager<MockIdentity>> {
        let scoring = SwarmScoringConfig::builder()
            .warn_threshold(-10.0)
            .disconnect_threshold(-20.0)
            .ban_threshold(-30.0)
            .build();
        PeerManager::new(
            &mock_identity(),
            PeerManagerConfig {
                scoring,
                ..Default::default()
            },
        )
    }

    #[test]
    fn test_report_peer_warn_crossing_emits_score_warning() {
        let pm = thresholds_manager();
        let overlay = test_overlay(1);
        pm.on_peer_connected(
            test_swarm_peer(1),
            SwarmNodeType::Client,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );
        let mut rx = pm.subscribe();

        // ConnectionSuccess left the score at +1; ProtocolError is -3 each.
        // Four reports reach -11, crossing the warn threshold once.
        for _ in 0..4 {
            pm.report_peer(
                &overlay,
                SwarmScoringEvent::ProtocolError,
                ReportSource::Protocol("test"),
            );
        }

        let events = drain_events(&mut rx);
        let warnings = events
            .iter()
            .filter(|e| matches!(e, PeerLifecycleEvent::ScoreWarning { overlay: o, .. } if *o == overlay))
            .count();
        assert_eq!(warnings, 1, "warn is edge-triggered: exactly one event");
        assert!(!pm.is_banned(&overlay));
        assert!(!pm.peer_is_in_backoff(&overlay));
    }

    #[test]
    fn test_report_peer_disconnect_crossing_emits_request_and_backoff() {
        let pm = thresholds_manager();
        let overlay = test_overlay(1);
        pm.on_peer_connected(
            test_swarm_peer(1),
            SwarmNodeType::Client,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );
        let mut rx = pm.subscribe();

        // +1 -> -20: seven ProtocolError reports cross the disconnect
        // threshold on the last one.
        for _ in 0..7 {
            pm.report_peer(
                &overlay,
                SwarmScoringEvent::ProtocolError,
                ReportSource::Topology,
            );
        }

        let events = drain_events(&mut rx);
        assert!(
            events.iter().any(|e| matches!(
                e,
                PeerLifecycleEvent::DisconnectRequested {
                    overlay: o,
                    reason: DisconnectCause::LowScore,
                } if *o == overlay
            )),
            "disconnect crossing must emit DisconnectRequested"
        );
        assert!(
            pm.peer_is_in_backoff(&overlay),
            "disconnect outcome must apply dial backoff"
        );
        assert!(!pm.is_banned(&overlay));
    }

    #[test]
    fn test_report_peer_ban_threshold_bans_once() {
        let pm = thresholds_manager();
        let overlay = test_overlay(1);
        pm.on_peer_connected(
            test_swarm_peer(1),
            SwarmNodeType::Client,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );
        let mut rx = pm.subscribe();

        // Drive well past the ban threshold; ban is level-triggered but the
        // ban action (and its event) must fire exactly once.
        for _ in 0..15 {
            pm.report_peer(
                &overlay,
                SwarmScoringEvent::ProtocolError,
                ReportSource::Accounting,
            );
        }

        assert!(pm.is_banned(&overlay));
        assert!(!pm.eligible_peers().contains(&overlay));
        let events = drain_events(&mut rx);
        let bans = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    PeerLifecycleEvent::Banned {
                        overlay: o,
                        reason: BanCause::LowScore,
                        ..
                    } if *o == overlay
                )
            })
            .count();
        assert_eq!(bans, 1, "repeated Ban outcomes must not re-emit Banned");
    }

    #[test]
    fn test_report_peer_unknown_overlay_is_dropped() {
        let pm = manager();
        // No entry exists; the report must be a no-op, not a panic or insert.
        pm.report_peer(
            &test_overlay(9),
            SwarmScoringEvent::MaliciousBehavior,
            ReportSource::Rpc,
        );
        assert!(pm.get_peer_score(&test_overlay(9)).is_none());
    }

    #[test]
    fn test_lifecycle_connect_disconnect_events_and_state() {
        let pm = manager();
        let overlay = test_overlay(1);
        let mut rx = pm.subscribe();

        pm.on_peer_connected(
            test_swarm_peer(1),
            SwarmNodeType::Storer,
            ConnectionDirection::Inbound,
            TrustLevel::LocalSubnet,
        );
        assert!(pm.is_connected(&overlay));
        assert!(pm.connected_since(&overlay).is_some());
        assert_eq!(
            pm.connection_direction(&overlay),
            Some(ConnectionDirection::Inbound)
        );
        assert_eq!(pm.trust_level(&overlay), TrustLevel::LocalSubnet);
        assert!(matches!(
            rx.try_recv(),
            Ok(PeerLifecycleEvent::Connected {
                overlay: o,
                node_type: SwarmNodeType::Storer,
            }) if o == overlay
        ));

        pm.on_peer_disconnected(&overlay, "test");
        assert!(!pm.is_connected(&overlay));
        assert!(pm.connected_since(&overlay).is_none());
        assert_eq!(pm.connection_direction(&overlay), None);
        // Trust describes the peer, not the connection: it survives until
        // the next handshake recomputes it.
        assert_eq!(pm.trust_level(&overlay), TrustLevel::LocalSubnet);
        let events = drain_events(&mut rx);
        assert!(events.iter().any(
            |e| matches!(e, PeerLifecycleEvent::Disconnected { overlay: o } if *o == overlay)
        ));
    }

    #[test]
    fn test_gossip_cannot_mutate_trust_or_node_type() {
        let pm = manager();
        let overlay = test_overlay(1);

        pm.on_peer_connected(
            test_swarm_peer(1),
            SwarmNodeType::Storer,
            ConnectionDirection::Outbound,
            TrustLevel::Trusted,
        );

        pm.store_discovered_peer(test_swarm_peer(1));

        assert_eq!(pm.node_type(&overlay), Some(SwarmNodeType::Storer));
        assert_eq!(pm.trust_level(&overlay), TrustLevel::Trusted);
    }

    #[test]
    fn test_subscriber_lag_drops_oldest_then_resumes() {
        let pm = manager();
        let overlay = test_overlay(1);
        let mut rx = pm.subscribe();

        for _ in 0..(LIFECYCLE_CHANNEL_CAPACITY + 16) {
            pm.on_peer_disconnected(&overlay, "test");
        }

        // The documented policy: a lagged receiver sees Lagged once, losing
        // the oldest events, then resumes from the oldest retained event.
        match rx.try_recv() {
            Err(broadcast::error::TryRecvError::Lagged(missed)) => {
                assert!(missed >= 1, "lag must report dropped events");
            }
            other => panic!("expected lagged receiver, got {other:?}"),
        }
        assert!(matches!(
            rx.try_recv(),
            Ok(PeerLifecycleEvent::Disconnected { .. })
        ));
    }

    #[test]
    fn test_memory_bounded() {
        let store: Arc<dyn NetPeerStore<StoredPeer>> =
            Arc::new(MemoryPeerStore::<StoredPeer>::new());
        let pm = manager_with_store(store, None);

        for n in 1..=200 {
            pm.store_discovered_peer(test_swarm_peer(n));
        }

        assert_eq!(pm.index().len(), 200);
        assert_eq!(pm.peers.len(), 0);
        assert_eq!(pm.eligible_count(), 200);
    }
}
