//! Peer manager holding the entire known peer set in memory.
//!
//! Every known peer lives in one `DashMap`; the `ProximityIndex` is a pure
//! bin-membership index over it with a per-bin admission cap. Persistence is
//! an optional identity-only snapshot ([`PeerSnapshot`]) written periodically
//! and on shutdown; reputation, bans, and dial backoff never survive a
//! restart. A crash loses at most one snapshot interval of newly discovered
//! peers, which bootnodes and hive gossip rediscover in seconds.

use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use dashmap::{DashMap, DashSet};
use metrics::{counter, gauge};
use tokio::sync::broadcast;
use tracing::{debug, warn};
use vertex_net_local::IpCapability;
use vertex_net_peer_registry::ConnectionDirection;
use vertex_net_peer_store::PeerSnapshotStore;
use vertex_swarm_api::{
    BanCause, PeerLifecycleEvent, ReportSource, SwarmIdentity, SwarmPeerResolver,
    SwarmScoringEvent, SwarmSpec,
};
use vertex_swarm_peer::{SwarmPeer, Timestamp, check_timestamp};
use vertex_swarm_peer_score::SwarmScoringConfig;
use vertex_swarm_primitives::{Bin, OverlayAddress, SwarmNodeType};

use crate::entry::{
    HealthState, PeerEntry, PeerSnapshot, TrustLevel, on_health_added, on_health_changed,
    on_health_removed, unix_timestamp_secs,
};
use crate::proximity_index::{AddError, ProximityIndex};
use crate::score_distribution::ScoreDistribution;

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
/// Carries the scoring policy, the per-bin admission cap, and the optional
/// snapshot store. `Default` yields an ephemeral manager: no store, nothing
/// survives shutdown.
#[derive(Clone)]
pub struct PeerManagerConfig {
    /// Peer scoring weights and ban/warn thresholds.
    pub scoring: SwarmScoringConfig,
    /// Maximum peers tracked per proximity bin.
    ///
    /// Bounds total memory at `max_per_bin * bins` peer records (a few MB at
    /// the defaults). Topology targets 3-35 connected peers per bin, so the
    /// default of 128 leaves ample headroom for unconnected dial candidates.
    pub max_per_bin: usize,
    /// Minimum time between periodic snapshots written by [`PeerManager::tick`].
    pub snapshot_interval: Duration,
    /// Snapshot persistence; `None` keeps the peer set memory-only.
    pub store: Option<Arc<dyn PeerSnapshotStore<PeerSnapshot>>>,
}

impl PeerManagerConfig {
    /// Default maximum peers per proximity bin.
    ///
    /// With topology routing targets of 3-35 connected peers per bin, 128
    /// gives 3.7-42x headroom for unconnected dial candidates.
    pub const DEFAULT_MAX_PER_BIN: usize = 128;

    /// Default minimum time between periodic snapshots (5 minutes).
    ///
    /// Trades snapshot-write frequency against how many freshly learned
    /// peers a crash can lose.
    pub const DEFAULT_SNAPSHOT_INTERVAL: Duration = Duration::from_secs(300);
}

impl Default for PeerManagerConfig {
    fn default() -> Self {
        Self {
            scoring: SwarmScoringConfig::default(),
            max_per_bin: Self::DEFAULT_MAX_PER_BIN,
            snapshot_interval: Self::DEFAULT_SNAPSHOT_INTERVAL,
            store: None,
        }
    }
}

/// Peer lifecycle manager with a single in-memory peer set.
///
/// All known peers live in the `DashMap`; the `ProximityIndex` tracks bin
/// membership and enforces the per-bin admission cap. With a snapshot store
/// configured, the set is loaded once at startup and written back
/// periodically (see [`Self::tick`]) and on shutdown (see [`Self::snapshot`]).
pub struct PeerManager<I: SwarmIdentity> {
    pub(crate) _identity: PhantomData<I>,
    /// Bin-membership index over the peer set (ALL known overlays).
    pub(crate) index: ProximityIndex,
    /// The entire known peer set.
    pub(crate) peers: DashMap<OverlayAddress, Arc<PeerEntry>>,
    /// Snapshot persistence (None for ephemeral/test mode).
    pub(crate) store: Option<Arc<dyn PeerSnapshotStore<PeerSnapshot>>>,
    /// O(1) ban checks. Starts empty on every startup: bans are runtime-only.
    pub(crate) banned_set: DashSet<OverlayAddress>,
    /// Scoring configuration.
    pub(crate) scoring_config: Arc<SwarmScoringConfig>,
    /// Minimum time between periodic snapshots.
    pub(crate) snapshot_interval: Duration,
    /// Unix seconds of the last periodic snapshot.
    pub(crate) last_snapshot: AtomicU64,
    /// Per-bucket gauge tracking of score distribution.
    pub(crate) score_distribution: Arc<ScoreDistribution>,
    /// Peer lifecycle event broadcast (see [`Self::subscribe`]).
    pub(crate) lifecycle_tx: broadcast::Sender<PeerLifecycleEvent>,
}

impl<I: SwarmIdentity> PeerManager<I> {
    /// Create a peer manager for `identity` from `config`.
    ///
    /// With `config.store` set, the peer set is loaded from the snapshot on
    /// construction; entries that would exceed the per-bin cap are dropped.
    /// The banned set always starts empty: bans are timed, runtime-only
    /// state that is re-earned in seconds.
    pub fn new(identity: &I, config: PeerManagerConfig) -> Arc<Self> {
        let PeerManagerConfig {
            scoring,
            max_per_bin,
            snapshot_interval,
            store,
        } = config;
        let local_overlay = identity.overlay_address();
        let max_po = identity.spec().max_po();
        let (lifecycle_tx, _) = broadcast::channel(LIFECYCLE_CHANNEL_CAPACITY);
        let pm = Arc::new(Self {
            _identity: PhantomData,
            index: ProximityIndex::new(local_overlay, max_po, max_per_bin),
            peers: DashMap::new(),
            store,
            banned_set: DashSet::new(),
            scoring_config: Arc::new(scoring),
            snapshot_interval,
            last_snapshot: AtomicU64::new(unix_timestamp_secs()),
            score_distribution: Arc::new(ScoreDistribution::new()),
            lifecycle_tx,
        });
        pm.load_from_store();
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
        self.peers.get(overlay).map(|entry| entry.node_type())
    }

    /// Get all peer overlays that are not banned and not in backoff.
    #[must_use]
    pub fn eligible_peers(&self) -> Vec<OverlayAddress> {
        self.peers
            .iter()
            .filter(|r| r.value().is_dialable())
            .map(|r| *r.key())
            .collect()
    }

    /// Count of peers that are not banned (O(1)).
    #[must_use]
    pub fn eligible_count(&self) -> usize {
        self.index.len().saturating_sub(self.banned_set.len())
    }

    /// Get all known Storer peers that aren't banned.
    #[must_use]
    pub fn known_storer_overlays(&self) -> Vec<OverlayAddress> {
        self.index
            .iter_by_proximity()
            .map(|(_, overlay)| overlay)
            .filter(|overlay| {
                !self.banned_set.contains(overlay)
                    && self
                        .peers
                        .get(overlay)
                        .is_some_and(|e| e.node_type() == SwarmNodeType::Storer)
            })
            .collect()
    }

    /// Get known storer overlays in a specific proximity bin (not banned).
    #[must_use]
    pub fn storer_overlays_in_bin(&self, bin: Bin, count: usize) -> Vec<OverlayAddress> {
        self.index.filter_bin(bin, count, |overlay| {
            !self.banned_set.contains(overlay)
                && self
                    .peers
                    .get(overlay)
                    .is_some_and(|e| e.node_type() == SwarmNodeType::Storer)
        })
    }

    /// Get SwarmPeer data for multiple overlays.
    #[must_use]
    pub fn get_swarm_peers(&self, overlays: &[OverlayAddress]) -> Vec<SwarmPeer> {
        overlays
            .iter()
            .filter_map(|o| self.peers.get(o).map(|e| e.swarm_peer()))
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
                let entry = self.peers.get(overlay)?;
                entry.is_dialable().then(|| entry.swarm_peer())
            })
            .collect()
    }

    /// Get dialable overlay addresses from a specific bin (not banned, not in backoff).
    pub fn dialable_overlays_in_bin(&self, bin: Bin, count: usize) -> Vec<OverlayAddress> {
        self.index.filter_bin(bin, count, |overlay| {
            !self.banned_set.contains(overlay)
                && self.peers.get(overlay).is_some_and(|e| e.is_dialable())
        })
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

    /// Get SwarmPeer for a single overlay.
    #[must_use]
    pub fn get_swarm_peer(&self, overlay: &OverlayAddress) -> Option<SwarmPeer> {
        self.peers.get(overlay).map(|e| e.swarm_peer())
    }

    /// Get a snapshot of all banned peer overlays.
    #[must_use]
    pub fn banned_set(&self) -> std::collections::HashSet<OverlayAddress> {
        self.banned_set.iter().map(|r| *r).collect()
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

    /// Check if peer is in backoff.
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
    /// peer's connection. Bans are runtime-only: they never persist across a
    /// restart and currently have no scheduled expiry within a session.
    pub fn ban(&self, overlay: &OverlayAddress, cause: BanCause, reason: Option<String>) {
        if !self.banned_set.insert(*overlay) {
            return; // Already banned
        }

        gauge!("peer_manager_banned_peers").increment(1.0);

        if let Some(entry) = self.peers.get(overlay)
            && !entry.is_banned()
        {
            let old_state = entry.health_state();
            warn!(?overlay, %cause, ?reason, "banning peer");
            entry.ban(reason);
            on_health_changed(old_state, HealthState::Banned);
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
    /// For known peers, updates addresses (preserving any handshake-confirmed
    /// node type). New peers go through per-bin admission: a full bin may
    /// replace its worst disconnected member, but never a connected one; if
    /// every slot is connected the newcomer is dropped.
    pub fn store_discovered_peer(&self, swarm_peer: SwarmPeer) -> OverlayAddress {
        let overlay = OverlayAddress::from(*swarm_peer.overlay());
        if let Some(entry) = self.peers.get(&overlay) {
            // Known peer - resolve the gossip timestamp against the stored
            // record before overwriting, then update addresses.
            if self.reject_stale_gossip(&overlay, swarm_peer.timestamp(), Some(entry.timestamp())) {
                return overlay;
            }
            entry.update_addresses(swarm_peer);
        } else if let Some(entry) = self.insert_peer(overlay, swarm_peer, SwarmNodeType::Client) {
            // The provisional refresh is dropped if a concurrent handshake
            // has already confirmed the node type.
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
    /// Confirms the handshake-asserted node type (from here on gossip cannot
    /// change it; only a later handshake may re-confirm a different value),
    /// records the connection state (connected-since, direction) and the
    /// topology-computed [`TrustLevel`] on the entry, emits
    /// [`PeerLifecycleEvent::Connected`], and reports the connection success
    /// through [`Self::report_peer`].
    pub fn on_peer_connected(
        &self,
        swarm_peer: SwarmPeer,
        node_type: SwarmNodeType,
        direction: ConnectionDirection,
        trust: TrustLevel,
    ) {
        let overlay = OverlayAddress::from(*swarm_peer.overlay());
        debug!(?overlay, ?node_type, %direction, %trust, "peer connected");

        let Some(entry) = self.insert_peer(overlay, swarm_peer, node_type) else {
            // Per-bin admission found only connected peers to displace.
            // Topology's own connection limits sit far below the bin cap, so
            // this is a pathological state worth surfacing.
            warn!(
                ?overlay,
                "dropping connected peer: bin full of connected peers"
            );
            return;
        };
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

    /// Stored [`TrustLevel`] for a peer (one atomic load on the entry).
    ///
    /// Defaults to [`TrustLevel::Normal`] for unknown peers; the level is
    /// process-local and recomputed at every handshake.
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

    /// Total peers currently held in memory.
    #[must_use]
    pub fn stored_count(&self) -> usize {
        self.peers.len()
    }

    /// Insert or update a peer, returning its entry.
    ///
    /// `node_type` only seeds new entries (as a provisional value); existing
    /// entries get their addresses refreshed and keep their node type.
    /// Callers apply the source-appropriate node type write on the returned
    /// entry: `confirm_node_type` for handshakes, `set_provisional_node_type`
    /// for gossip.
    ///
    /// Returns `None` when the peer's bin is at capacity and every slot is
    /// held by a connected peer (see [`Self::admit`]).
    fn insert_peer(
        &self,
        overlay: OverlayAddress,
        peer: SwarmPeer,
        node_type: SwarmNodeType,
    ) -> Option<Arc<PeerEntry>> {
        use dashmap::mapref::entry::Entry;

        if let Some(existing) = self.peers.get(&overlay) {
            existing.update_addresses(peer);
            return Some(Arc::clone(existing.value()));
        }

        // Admission runs before taking the map entry lock: replacement may
        // remove another peer from the same DashMap shard.
        if !self.admit(overlay) {
            return None;
        }

        match self.peers.entry(overlay) {
            Entry::Occupied(e) => {
                // A concurrent insert won the race; refresh addresses.
                e.get().update_addresses(peer);
                Some(Arc::clone(e.get()))
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
                gauge!("peer_manager_total_peers").set(self.index.len() as f64);
                self.score_distribution.on_peer_added(initial_score);
                on_health_added(HealthState::Healthy);
                Some(cloned)
            }
        }
    }

    /// Admit `overlay` into its proximity bin, replacing a disconnected
    /// member if the bin is full.
    ///
    /// Replacement policy: never evict a connected peer; otherwise replace
    /// the worst disconnected record, preferring stale entries, then the
    /// lowest score. Returns `false` when every slot is held by a connected
    /// peer (the newcomer is rejected).
    fn admit(&self, overlay: OverlayAddress) -> bool {
        match self.index.add(overlay) {
            Ok(()) | Err(AddError::AlreadyPresent) => true,
            Err(AddError::BinFull) => {
                let bin = self.index.bin_for(&overlay);
                let Some(victim) = self.find_replaceable(bin) else {
                    counter!("peer_manager_admission_rejected_total").increment(1);
                    debug!(?overlay, %bin, "bin full of connected peers; rejecting newcomer");
                    return false;
                };
                debug!(?overlay, ?victim, %bin, "replacing worst disconnected peer in full bin");
                self.remove_peer(&victim);
                // A concurrent admission may have refilled the slot; treat
                // that as a lost race and drop the newcomer.
                matches!(
                    self.index.add(overlay),
                    Ok(()) | Err(AddError::AlreadyPresent)
                )
            }
        }
    }

    /// Pick the replacement victim in `bin`: the worst disconnected member,
    /// stale entries first, then lowest score. Connected peers are never
    /// candidates.
    fn find_replaceable(&self, bin: Bin) -> Option<OverlayAddress> {
        let mut best: Option<(OverlayAddress, bool, f64)> = None;
        for overlay in self.index.peers_in_bin(bin) {
            let Some(entry) = self.peers.get(&overlay) else {
                // Index entry without a record: replace immediately.
                return Some(overlay);
            };
            if entry.is_connected() {
                continue;
            }
            let stale = entry.is_stale();
            let score = entry.score();
            let better = match &best {
                None => true,
                Some((_, best_stale, best_score)) => {
                    (stale && !best_stale) || (stale == *best_stale && score < *best_score)
                }
            };
            if better {
                best = Some((overlay, stale, score));
            }
        }
        best.map(|(overlay, _, _)| overlay)
    }

    /// Fully remove a peer from all data structures (index, peer set, banned set).
    pub(crate) fn remove_peer(&self, overlay: &OverlayAddress) {
        if let Some((_, entry)) = self.peers.remove(overlay) {
            self.score_distribution.on_peer_removed(entry.score());
            on_health_removed(entry.health_state());
        }
        if self.index.remove(overlay) {
            gauge!("peer_manager_total_peers").set(self.index.len() as f64);
        }
        if self.banned_set.remove(overlay).is_some() {
            gauge!("peer_manager_banned_peers").decrement(1.0);
        }
    }
}

impl<I: SwarmIdentity> SwarmPeerResolver for PeerManager<I> {
    type Peer = SwarmPeer;

    fn get_swarm_peer(&self, overlay: &OverlayAddress) -> Option<SwarmPeer> {
        self.peers.get(overlay).map(|e| e.swarm_peer())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_net_peer_store::MemoryPeerStore;
    use vertex_swarm_api::DisconnectCause;
    use vertex_swarm_test_utils::{
        MockIdentity, make_swarm_peer_minimal, test_overlay, test_swarm_peer,
        test_swarm_peer_with_timestamp,
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
        store: Arc<dyn PeerSnapshotStore<PeerSnapshot>>,
    ) -> Arc<PeerManager<MockIdentity>> {
        PeerManager::new(
            &mock_identity(),
            PeerManagerConfig {
                store: Some(store),
                ..Default::default()
            },
        )
    }

    fn memory_store() -> Arc<dyn PeerSnapshotStore<PeerSnapshot>> {
        Arc::new(MemoryPeerStore::<PeerSnapshot>::new())
    }

    fn connect(pm: &PeerManager<MockIdentity>, n: u8, node_type: SwarmNodeType) {
        pm.on_peer_connected(
            test_swarm_peer(n),
            node_type,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );
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
        let overlay = test_overlay(1);

        connect(&pm, 1, SwarmNodeType::Storer);
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

        connect(&pm, 1, SwarmNodeType::Client);
        assert!(pm.eligible_peers().contains(&overlay));
    }

    #[test]
    fn test_ban() {
        let pm = manager();
        let overlay = test_overlay(1);

        connect(&pm, 1, SwarmNodeType::Client);
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

        connect(&pm, 1, SwarmNodeType::Storer);
        connect(&pm, 2, SwarmNodeType::Client);
        connect(&pm, 3, SwarmNodeType::Storer);
        connect(&pm, 4, SwarmNodeType::Client);

        pm.ban(&test_overlay(1), BanCause::Requested, None);

        let storers = pm.known_storer_overlays();
        assert_eq!(storers.len(), 1);
        assert!(storers.contains(&test_overlay(3)));
    }

    #[test]
    fn test_get_swarm_peers() {
        let pm = manager();

        for n in 1..=5 {
            connect(&pm, n, SwarmNodeType::Storer);
        }

        let overlays = vec![test_overlay(1), test_overlay(3), test_overlay(5)];
        let peers = pm.get_swarm_peers(&overlays);

        assert_eq!(peers.len(), 3);
    }

    #[test]
    fn test_get_swarm_peers_missing() {
        let pm = manager();

        connect(&pm, 1, SwarmNodeType::Storer);

        let overlays = vec![test_overlay(1), test_overlay(99)];
        let peers = pm.get_swarm_peers(&overlays);

        assert_eq!(peers.len(), 1);
    }

    #[test]
    fn test_node_type_variants() {
        let pm = manager();

        connect(&pm, 1, SwarmNodeType::Bootnode);
        connect(&pm, 2, SwarmNodeType::Client);
        connect(&pm, 3, SwarmNodeType::Storer);

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
    fn test_dialable_in_bin() {
        let pm = manager();

        // First bytes 0x80, 0xc0, 0xa0: all bin 0 relative to the zero local
        // overlay.
        for byte in [0x80, 0xc0, 0xa0] {
            pm.store_discovered_peer(make_swarm_peer_minimal(byte));
        }

        let p1 = OverlayAddress::from(*make_swarm_peer_minimal(0x80).overlay());
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
    fn test_snapshot_roundtrip_memory() {
        let store = memory_store();
        let pm1 = manager_with_store(Arc::clone(&store));

        for n in 1..=5 {
            connect(&pm1, n, SwarmNodeType::Storer);
        }
        pm1.snapshot();

        let pm2 = manager_with_store(store);
        assert_eq!(pm2.index().len(), 5);
        for n in 1..=5 {
            assert!(pm2.get_swarm_peer(&test_overlay(n)).is_some());
            assert_eq!(pm2.node_type(&test_overlay(n)), Some(SwarmNodeType::Storer));
        }
    }

    #[test]
    fn test_snapshot_roundtrip_redb() {
        let db = vertex_storage_redb::RedbDatabase::in_memory()
            .unwrap()
            .into_arc();
        let db_store = Arc::new(crate::snapshot_store::DbPeerSnapshotStore::new(db));
        db_store.init().unwrap();
        let store: Arc<dyn PeerSnapshotStore<PeerSnapshot>> = db_store;

        let pm1 = manager_with_store(Arc::clone(&store));
        for n in 1..=5 {
            connect(&pm1, n, SwarmNodeType::Storer);
        }
        pm1.snapshot();

        let pm2 = manager_with_store(store);
        assert_eq!(pm2.index().len(), 5);
        for n in 1..=5 {
            assert!(pm2.get_swarm_peer(&test_overlay(n)).is_some());
            assert_eq!(pm2.node_type(&test_overlay(n)), Some(SwarmNodeType::Storer));
        }
    }

    #[test]
    fn test_startup_drops_over_cap_snapshot_entries() {
        let store = memory_store();
        // Persist four bin-0 peers directly.
        let records: Vec<PeerSnapshot> = [0x80u8, 0xc0, 0xa0, 0xb0]
            .into_iter()
            .map(|byte| PeerSnapshot {
                peer: make_swarm_peer_minimal(byte),
                node_type: SwarmNodeType::Client,
                last_seen: 1000,
            })
            .collect();
        store.store(&records).unwrap();

        let pm = PeerManager::new(
            &mock_identity(),
            PeerManagerConfig {
                max_per_bin: 2,
                store: Some(store),
                ..Default::default()
            },
        );

        assert_eq!(pm.index().len(), 2, "over-cap snapshot entries dropped");
        assert_eq!(pm.stored_count(), 2);
        assert_eq!(pm.index().bin_size(Bin::new(0).unwrap()), 2);
    }

    #[test]
    fn test_ban_does_not_persist_across_restart() {
        let store = memory_store();
        let pm1 = manager_with_store(Arc::clone(&store));

        connect(&pm1, 1, SwarmNodeType::Client);
        // Drive the score down before banning so the reset is observable.
        for _ in 0..3 {
            pm1.report_peer(
                &test_overlay(1),
                SwarmScoringEvent::ProtocolError,
                ReportSource::Topology,
            );
        }
        pm1.ban(&test_overlay(1), BanCause::Requested, Some("bad".into()));
        pm1.snapshot();

        let pm2 = manager_with_store(store);
        assert!(
            pm2.get_swarm_peer(&test_overlay(1)).is_some(),
            "peer identity survives the restart"
        );
        assert!(!pm2.is_banned(&test_overlay(1)), "bans are runtime-only");
        assert_eq!(pm2.banned_count(), 0);
        assert_eq!(
            pm2.get_peer_score(&test_overlay(1)),
            Some(0.0),
            "scores reset on restart"
        );
        assert!(pm2.eligible_peers().contains(&test_overlay(1)));
    }

    #[test]
    fn test_tick_snapshots_when_due() {
        let store = memory_store();
        let pm = PeerManager::new(
            &mock_identity(),
            PeerManagerConfig {
                snapshot_interval: Duration::from_secs(300),
                store: Some(Arc::clone(&store)),
                ..Default::default()
            },
        );
        connect(&pm, 1, SwarmNodeType::Client);

        let start = unix_timestamp_secs();
        // Not due yet: nothing written.
        pm.tick(start + 10);
        assert!(store.load().unwrap().is_empty());

        // Due: the full set is written.
        pm.tick(start + 301);
        assert_eq!(store.load().unwrap().len(), 1);

        // Immediately after, not due again.
        connect(&pm, 2, SwarmNodeType::Client);
        pm.tick(start + 302);
        assert_eq!(store.load().unwrap().len(), 1);

        // Due again after another interval.
        pm.tick(start + 602);
        assert_eq!(store.load().unwrap().len(), 2);
    }

    #[test]
    fn test_tick_purges_stale_peers() {
        let pm = manager();
        connect(&pm, 1, SwarmNodeType::Client);
        pm.store_discovered_peer(test_swarm_peer(2));

        // 48 consecutive failures marks the peer stale regardless of
        // last_seen.
        for _ in 0..48 {
            pm.record_dial_failure(&test_overlay(2));
        }

        pm.tick(unix_timestamp_secs());

        assert!(pm.get_swarm_peer(&test_overlay(2)).is_none());
        assert!(!pm.index().exists(&test_overlay(2)));
        assert!(pm.get_swarm_peer(&test_overlay(1)).is_some());
    }

    #[test]
    fn test_bin_full_replaces_lowest_score_disconnected() {
        let pm = PeerManager::new(
            &mock_identity(),
            PeerManagerConfig {
                max_per_bin: 2,
                ..Default::default()
            },
        );

        // Two disconnected bin-0 peers.
        pm.store_discovered_peer(make_swarm_peer_minimal(0x80));
        pm.store_discovered_peer(make_swarm_peer_minimal(0xc0));
        let low = OverlayAddress::from(*make_swarm_peer_minimal(0x80).overlay());
        let high = OverlayAddress::from(*make_swarm_peer_minimal(0xc0).overlay());

        // Drive one peer's score down.
        for _ in 0..2 {
            pm.report_peer(
                &low,
                SwarmScoringEvent::ProtocolError,
                ReportSource::Topology,
            );
        }

        // Newcomer must displace the lowest-score disconnected peer.
        pm.store_discovered_peer(make_swarm_peer_minimal(0xa0));
        let newcomer = OverlayAddress::from(*make_swarm_peer_minimal(0xa0).overlay());

        assert!(pm.get_swarm_peer(&newcomer).is_some());
        assert!(pm.get_swarm_peer(&high).is_some());
        assert!(pm.get_swarm_peer(&low).is_none(), "lowest score replaced");
        assert_eq!(pm.index().bin_size(Bin::new(0).unwrap()), 2);
    }

    #[test]
    fn test_bin_full_replaces_stale_before_low_score() {
        let pm = PeerManager::new(
            &mock_identity(),
            PeerManagerConfig {
                max_per_bin: 2,
                ..Default::default()
            },
        );

        pm.store_discovered_peer(make_swarm_peer_minimal(0x80));
        pm.store_discovered_peer(make_swarm_peer_minimal(0xc0));
        let stale = OverlayAddress::from(*make_swarm_peer_minimal(0x80).overlay());
        let low_score = OverlayAddress::from(*make_swarm_peer_minimal(0xc0).overlay());

        // `stale` accumulates 48 dial failures (stale, but each failure is
        // score-neutral); `low_score` has a worse score but stays fresh.
        for _ in 0..48 {
            pm.record_dial_failure(&stale);
        }
        for _ in 0..3 {
            pm.report_peer(
                &low_score,
                SwarmScoringEvent::ProtocolError,
                ReportSource::Topology,
            );
        }

        pm.store_discovered_peer(make_swarm_peer_minimal(0xa0));
        let newcomer = OverlayAddress::from(*make_swarm_peer_minimal(0xa0).overlay());

        assert!(pm.get_swarm_peer(&newcomer).is_some());
        assert!(pm.get_swarm_peer(&stale).is_none(), "stale replaced first");
        assert!(pm.get_swarm_peer(&low_score).is_some());
    }

    #[test]
    fn test_bin_full_never_evicts_connected() {
        let pm = PeerManager::new(
            &mock_identity(),
            PeerManagerConfig {
                max_per_bin: 2,
                ..Default::default()
            },
        );

        // Fill bin 0 with two connected peers.
        for byte in [0x80, 0xc0] {
            pm.on_peer_connected(
                make_swarm_peer_minimal(byte),
                SwarmNodeType::Client,
                ConnectionDirection::Outbound,
                TrustLevel::Normal,
            );
        }

        // Newcomer is rejected: every slot is connected.
        pm.store_discovered_peer(make_swarm_peer_minimal(0xa0));
        let newcomer = OverlayAddress::from(*make_swarm_peer_minimal(0xa0).overlay());

        assert!(pm.get_swarm_peer(&newcomer).is_none());
        assert!(!pm.index().exists(&newcomer));
        for byte in [0x80, 0xc0] {
            let overlay = OverlayAddress::from(*make_swarm_peer_minimal(byte).overlay());
            assert!(pm.get_swarm_peer(&overlay).is_some());
        }
    }

    #[test]
    fn test_bin_full_mixed_replaces_only_disconnected() {
        let pm = PeerManager::new(
            &mock_identity(),
            PeerManagerConfig {
                max_per_bin: 2,
                ..Default::default()
            },
        );

        // One connected, one disconnected with a better score than the
        // connected peer would have.
        pm.on_peer_connected(
            make_swarm_peer_minimal(0x80),
            SwarmNodeType::Client,
            ConnectionDirection::Outbound,
            TrustLevel::Normal,
        );
        pm.store_discovered_peer(make_swarm_peer_minimal(0xc0));
        let connected = OverlayAddress::from(*make_swarm_peer_minimal(0x80).overlay());
        let disconnected = OverlayAddress::from(*make_swarm_peer_minimal(0xc0).overlay());

        pm.store_discovered_peer(make_swarm_peer_minimal(0xa0));
        let newcomer = OverlayAddress::from(*make_swarm_peer_minimal(0xa0).overlay());

        assert!(pm.get_swarm_peer(&connected).is_some(), "connected kept");
        assert!(pm.get_swarm_peer(&newcomer).is_some());
        assert!(pm.get_swarm_peer(&disconnected).is_none());
    }

    #[test]
    fn test_store_discovered_peer_preserves_node_type() {
        let pm = manager();
        let swarm_peer = test_swarm_peer(1);
        let overlay = test_overlay(1);

        connect(&pm, 1, SwarmNodeType::Storer);
        assert_eq!(pm.node_type(&overlay), Some(SwarmNodeType::Storer));

        pm.store_discovered_peer(swarm_peer);
        assert_eq!(pm.node_type(&overlay), Some(SwarmNodeType::Storer));
    }

    #[test]
    fn test_reconnect_handshake_reconfirms_node_type() {
        let pm = PeerManager::new(&mock_identity(), PeerManagerConfig::default());
        let overlay = test_overlay(1);

        connect(&pm, 1, SwarmNodeType::Client);
        assert_eq!(pm.node_type(&overlay), Some(SwarmNodeType::Client));

        // Gossip cannot change the confirmed type.
        pm.store_discovered_peer(test_swarm_peer(1));
        assert_eq!(pm.node_type(&overlay), Some(SwarmNodeType::Client));

        // The node upgraded between sessions; the new handshake re-confirms.
        connect(&pm, 1, SwarmNodeType::Storer);
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

        connect(&pm, 1, SwarmNodeType::Storer);
        connect(&pm, 2, SwarmNodeType::Storer);

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
    fn test_banned_set_o1() {
        let pm = manager();

        connect(&pm, 1, SwarmNodeType::Client);
        pm.ban(&test_overlay(1), BanCause::Requested, None);

        assert!(pm.is_banned(&test_overlay(1)));
        assert!(!pm.is_banned(&test_overlay(2)));
        assert_eq!(pm.banned_count(), 1);
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
    fn test_concurrent_gossip_and_queries() {
        use std::thread;

        let pm = manager();

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

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(pm.index().len(), 100);
        assert_eq!(pm.stored_count(), 100);
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
        connect(&pm, 1, SwarmNodeType::Client);
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
        connect(&pm, 1, SwarmNodeType::Client);
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
        connect(&pm, 1, SwarmNodeType::Client);
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
}
