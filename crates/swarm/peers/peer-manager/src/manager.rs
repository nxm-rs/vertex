//! Peer manager holding the entire known peer set in memory.
//!
//! Every known peer lives in one `DashMap`; the `ProximityIndex` is a pure
//! bin-membership index over it with a per-bin admission cap. Persistence is
//! an optional identity-only snapshot ([`PeerSnapshot`]) written periodically
//! and on shutdown; reputation, bans, and dial backoff never survive a
//! restart. A crash loses at most one snapshot interval of newly discovered
//! peers, which bootnodes and hive gossip rediscover in seconds.

use std::marker::PhantomData;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use dashmap::DashMap;
use metrics::{counter, gauge};
use parking_lot::Mutex;
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
use crate::ip_tracker::{IpGroup, IpTracker, IpTrackerConfig, RecordOutcome};
use crate::proximity_index::{AddError, ProximityIndex};
use crate::score_distribution::ScoreDistribution;

/// Outcome of a [`PeerManager::on_peer_connected`] admission decision.
///
/// The peer manager owns the per-IP and per-bin admission checks, but it is
/// the topology caller that holds the libp2p connection and the routing
/// table. Returning the outcome lets the caller tear down a rejected
/// connection (push `CloseConnection`) and skip the
/// routing/gossip/`PeerReady` wiring, so a rejection can never leave a
/// half-open connection that routing believes is live. `Admitted` is the
/// only outcome that records the peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionAdmission {
    /// The peer was recorded and connected.
    Admitted,
    /// Rejected by the live per-IP concurrent-connection cap.
    RejectedIpCap,
    /// Rejected because the Kademlia bin held only connected peers and none
    /// could be displaced.
    RejectedBinFull,
}

impl ConnectionAdmission {
    /// Whether the peer was admitted and recorded.
    #[must_use]
    pub fn is_admitted(self) -> bool {
        matches!(self, ConnectionAdmission::Admitted)
    }
}

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
    /// How long a timed ban lasts before [`PeerManager::tick`] lifts it.
    ///
    /// Applies to every ban except [`PeerManager::ban_permanent`]. On expiry
    /// the peer's score is reset to the disconnect threshold, so it must
    /// behave to climb back; it is not forgiven to neutral.
    pub ban_duration: Duration,
    /// Snapshot persistence; `None` keeps the peer set memory-only.
    pub store: Option<Arc<dyn PeerSnapshotStore<PeerSnapshot>>>,
    /// IP association tracking thresholds for identity-cycling detection.
    pub ip_tracker: IpTrackerConfig,
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

    /// Default timed ban duration (12 hours).
    ///
    /// Long enough that a misbehaving peer cannot grind through ban cycles
    /// cheaply, short enough that a transiently broken peer is not lost for
    /// good; bans never survive a restart either way.
    pub const DEFAULT_BAN_DURATION: Duration = Duration::from_secs(12 * 3600);
}

impl Default for PeerManagerConfig {
    fn default() -> Self {
        Self {
            scoring: SwarmScoringConfig::default(),
            max_per_bin: Self::DEFAULT_MAX_PER_BIN,
            snapshot_interval: Self::DEFAULT_SNAPSHOT_INTERVAL,
            ban_duration: Self::DEFAULT_BAN_DURATION,
            store: None,
            ip_tracker: IpTrackerConfig::default(),
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
    /// O(1) ban checks, mapping each banned overlay to its ban expiry in
    /// unix seconds (`None` for a permanent ban). Starts empty on every
    /// startup: bans are runtime-only.
    pub(crate) banned_set: DashMap<OverlayAddress, Option<u64>>,
    /// Scoring configuration.
    pub(crate) scoring_config: Arc<SwarmScoringConfig>,
    /// Minimum time between periodic snapshots.
    pub(crate) snapshot_interval: Duration,
    /// Duration of a timed ban.
    pub(crate) ban_duration: Duration,
    /// Unix seconds of the last periodic snapshot.
    pub(crate) last_snapshot: AtomicU64,
    /// Per-bucket gauge tracking of score distribution.
    pub(crate) score_distribution: Arc<ScoreDistribution>,
    /// Peer lifecycle event broadcast (see [`Self::subscribe`]).
    pub(crate) lifecycle_tx: broadcast::Sender<PeerLifecycleEvent>,
    /// Remote-IP association tracking for identity-cycling detection.
    ///
    /// One short lock per handshake completion and overlay removal; never
    /// touched on per-message paths.
    pub(crate) ip_tracker: Mutex<IpTracker>,
    /// Live concurrent-connection count per IP group.
    ///
    /// Incremented at handshake completion ([`Self::on_peer_connected`])
    /// and decremented on disconnect ([`Self::on_peer_disconnected`]) for
    /// peers below [`TrustLevel::LocalSubnet`]. Read under the per-shard
    /// `DashMap` lock to enforce [`Self::max_connections_per_ip`].
    pub(crate) live_ip_connections: DashMap<IpGroup, usize>,
    /// IP group each currently-counted overlay was admitted from.
    ///
    /// `on_peer_disconnected` only receives the overlay, so the admitting
    /// IP group is remembered here to decrement [`Self::live_ip_connections`]
    /// on disconnect. Only overlays that counted toward the cap (below
    /// `LocalSubnet` trust, with a known remote IP) are recorded.
    pub(crate) connection_ip_group: DashMap<OverlayAddress, IpGroup>,
    /// Live per-IP concurrent-connection admission cap; `None` is unlimited.
    pub(crate) max_connections_per_ip: Option<usize>,
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
            ban_duration,
            store,
            ip_tracker,
        } = config;
        let local_overlay = identity.overlay_address();
        let max_po = identity.spec().max_po();
        let (lifecycle_tx, _) = broadcast::channel(LIFECYCLE_CHANNEL_CAPACITY);
        let max_connections_per_ip = ip_tracker.max_connections_per_ip;
        let pm = Arc::new(Self {
            _identity: PhantomData,
            index: ProximityIndex::new(local_overlay, max_po, max_per_bin),
            peers: DashMap::new(),
            store,
            banned_set: DashMap::new(),
            scoring_config: Arc::new(scoring),
            snapshot_interval,
            ban_duration,
            last_snapshot: AtomicU64::new(unix_timestamp_secs()),
            score_distribution: Arc::new(ScoreDistribution::new()),
            lifecycle_tx,
            ip_tracker: Mutex::new(IpTracker::new(ip_tracker)),
            live_ip_connections: DashMap::new(),
            connection_ip_group: DashMap::new(),
            max_connections_per_ip,
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
                !self.banned_set.contains_key(overlay)
                    && self
                        .peers
                        .get(overlay)
                        .is_some_and(|e| e.node_type() == SwarmNodeType::Storer)
            })
            .collect()
    }

    /// Iterate known storer overlays in a specific proximity bin (not banned).
    ///
    /// Lazy over a snapshot of the bin's membership: the node-type and ban
    /// checks run per item as the caller advances, so callers take only
    /// what they need.
    pub fn storer_overlays_in_bin(&self, bin: Bin) -> impl Iterator<Item = OverlayAddress> + '_ {
        self.index
            .peers_in_bin(bin)
            .into_iter()
            .filter(move |overlay| {
                !self.banned_set.contains_key(overlay)
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
                if self.banned_set.contains_key(overlay) {
                    return None;
                }
                let entry = self.peers.get(overlay)?;
                entry.is_dialable().then(|| entry.swarm_peer())
            })
            .collect()
    }

    /// Iterate dialable overlay addresses in a specific bin (not banned, not
    /// in backoff).
    pub fn dialable_overlays_in_bin(&self, bin: Bin) -> impl Iterator<Item = OverlayAddress> + '_ {
        self.dialable_overlays_in_bin_excluding(bin, |_| false)
    }

    /// Iterate dialable overlay addresses in a bin, skipping excluded overlays.
    ///
    /// `exclude` lets the caller remove overlays that are dialable but useless
    /// as candidates, typically peers it is already connected to. The iterator
    /// is lazy over a snapshot of the bin's membership: exclusion, ban, and
    /// dialability checks run per item as the caller advances, so callers take
    /// only what they need and never pay for the rest of the bin.
    pub fn dialable_overlays_in_bin_excluding<'a>(
        &'a self,
        bin: Bin,
        exclude: impl Fn(&OverlayAddress) -> bool + 'a,
    ) -> impl Iterator<Item = OverlayAddress> + 'a {
        self.index
            .peers_in_bin(bin)
            .into_iter()
            .filter(move |overlay| {
                !self.banned_set.contains_key(overlay)
                    && !exclude(overlay)
                    && self.peers.get(overlay).is_some_and(|e| e.is_dialable())
            })
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
        self.banned_set.iter().map(|r| *r.key()).collect()
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

    /// Check if peer is banned (O(1) via the banned map).
    #[must_use]
    pub fn is_banned(&self, overlay: &OverlayAddress) -> bool {
        self.banned_set.contains_key(overlay)
    }

    /// Number of currently banned peers (O(1)).
    #[must_use]
    pub fn banned_count(&self) -> usize {
        self.banned_set.len()
    }

    /// Ban a peer for [`PeerManagerConfig::ban_duration`] and emit
    /// [`PeerLifecycleEvent::Banned`] with the expiry.
    ///
    /// Topology subscribes to the lifecycle stream and closes the banned
    /// peer's connection. [`Self::tick`] lifts the ban once the expiry
    /// passes, resetting the score to the disconnect threshold and emitting
    /// [`PeerLifecycleEvent::Unbanned`]. Banning an already banned peer is a
    /// no-op: the original expiry stands and no second event is emitted.
    /// Bans are runtime-only and never persist across a restart.
    pub fn ban(&self, overlay: &OverlayAddress, cause: BanCause, reason: Option<String>) {
        let until = unix_timestamp_secs() + self.ban_duration.as_secs();
        self.ban_with_expiry(overlay, cause, reason, Some(until));
    }

    /// Ban a peer with no scheduled expiry; [`Self::tick`] never lifts it,
    /// so only a restart clears it (bans never persist).
    ///
    /// Reserved for operator-initiated bans; the operator surface that calls
    /// this arrives with the gRPC work.
    pub fn ban_permanent(&self, overlay: &OverlayAddress, cause: BanCause, reason: Option<String>) {
        self.ban_with_expiry(overlay, cause, reason, None);
    }

    fn ban_with_expiry(
        &self,
        overlay: &OverlayAddress,
        cause: BanCause,
        reason: Option<String>,
        until: Option<u64>,
    ) {
        use dashmap::mapref::entry::Entry;

        match self.banned_set.entry(*overlay) {
            // Already banned: keep the original expiry, emit nothing.
            Entry::Occupied(_) => return,
            Entry::Vacant(vacant) => {
                vacant.insert(until);
            }
        }

        gauge!("peer_manager_banned_peers").increment(1.0);

        if let Some(entry) = self.peers.get(overlay)
            && !entry.is_banned()
        {
            let old_state = entry.health_state();
            warn!(?overlay, %cause, ?reason, until, "banning peer");
            entry.ban(reason);
            on_health_changed(old_state, HealthState::Banned);
        }

        self.emit(PeerLifecycleEvent::Banned {
            overlay: *overlay,
            until,
            reason: cause,
        });
    }

    /// Lift a peer's ban: clear the ban state, reset the score to the
    /// disconnect threshold, and emit [`PeerLifecycleEvent::Unbanned`].
    ///
    /// The score reset means an unbanned peer must behave to climb back; it
    /// is not forgiven to neutral. The event is emitted exactly once per
    /// ban (guarded by the banned-set removal).
    pub(crate) fn unban(&self, overlay: &OverlayAddress) {
        if let Some(entry) = self.peers.get(overlay) {
            let old_state = entry.health_state();
            entry.clear_ban();
            let (old_score, new_score) =
                entry.reset_score(self.scoring_config.disconnect_threshold());
            self.score_distribution
                .on_score_changed(old_score, new_score);
            on_health_changed(old_state, entry.health_state());
        }
        if self.banned_set.remove(overlay).is_some() {
            gauge!("peer_manager_banned_peers").decrement(1.0);
            self.emit(PeerLifecycleEvent::Unbanned { overlay: *overlay });
        }
    }

    /// Subscribe to the peer lifecycle event stream.
    ///
    /// The stream carries every [`PeerLifecycleEvent`]: connects,
    /// disconnects, score warnings, disconnect requests, bans, and unbans.
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
    /// node type and the verified bit: a record refresh for a verified peer
    /// needs only the signature validation already done at intake, never a
    /// dial). New peers go through per-bin admission: a full bin may
    /// replace its worst disconnected member, but never a connected one; if
    /// every slot is connected the newcomer is dropped. Admitted peers start
    /// unverified and dialable; candidate selection may dial them, and the
    /// first completed handshake verifies the record in the same round trip.
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
    /// marks the entry verified (the completed handshake IS the verification
    /// of a gossip-admitted record: overlay, signature, and multiaddrs all
    /// come from the peer itself), records the connection state
    /// (connected-since, direction) and the topology-computed [`TrustLevel`]
    /// on the entry, emits [`PeerLifecycleEvent::Connected`], and reports
    /// the connection success through [`Self::report_peer`].
    ///
    /// `remote_ip` is the IP the connection actually came from (not a
    /// gossiped or self-asserted address) and feeds IP association
    /// tracking; see [`Self::overlays_seen_from_ip`]. Peers trusted at
    /// [`TrustLevel::LocalSubnet`] or above are exempt: several nodes on
    /// one home LAN share an IP legitimately and must never trip the
    /// cycling detector.
    ///
    /// Returns the [`ConnectionAdmission`] outcome so the caller can tear
    /// down a rejected connection instead of leaving it half-open: a
    /// rejection records nothing here, so the caller must not wire the peer
    /// into routing or gossip.
    pub fn on_peer_connected(
        &self,
        swarm_peer: SwarmPeer,
        node_type: SwarmNodeType,
        direction: ConnectionDirection,
        trust: TrustLevel,
        remote_ip: Option<IpAddr>,
    ) -> ConnectionAdmission {
        let overlay = OverlayAddress::from(*swarm_peer.overlay());
        debug!(?overlay, ?node_type, %direction, %trust, "peer connected");

        // Per-IP concurrent-connection admission, before anything is
        // recorded. Peers at LocalSubnet trust or above are exempt (a home
        // LAN shares one IP legitimately); `None` cap is unlimited. The
        // reserved slot is released if downstream admission then drops the
        // peer.
        let counted_ip_group = if let Some(ip) = remote_ip
            && trust < TrustLevel::LocalSubnet
        {
            if !self.try_admit_ip_connection(overlay, ip) {
                counter!("peer_manager_ip_connection_rejected_total").increment(1);
                warn!(
                    ?overlay,
                    %ip,
                    cap = ?self.max_connections_per_ip,
                    "dropping connected peer: per-IP connection cap reached"
                );
                return ConnectionAdmission::RejectedIpCap;
            }
            Some(IpGroup::from(ip))
        } else {
            None
        };

        let Some(entry) = self.insert_peer(overlay, swarm_peer, node_type) else {
            // Per-bin admission found only connected peers to displace.
            // Topology's own connection limits sit far below the bin cap, so
            // this is a pathological state worth surfacing.
            if let Some(group) = counted_ip_group {
                self.connection_ip_group.remove(&overlay);
                self.release_ip_connection_for_group(group);
            }
            warn!(
                ?overlay,
                "dropping connected peer: bin full of connected peers"
            );
            return ConnectionAdmission::RejectedBinFull;
        };
        entry.confirm_node_type(node_type);
        if entry.mark_verified() {
            gauge!("peer_manager_unverified_peers").decrement(1.0);
        }
        let old_state = entry.health_state();
        entry.set_connected(direction, trust);
        on_health_changed(old_state, entry.health_state());

        self.emit(PeerLifecycleEvent::Connected { overlay, node_type });
        self.report_peer(
            &overlay,
            SwarmScoringEvent::ConnectionSuccess { latency: None },
            ReportSource::Topology,
        );

        if let Some(ip) = remote_ip
            && trust < TrustLevel::LocalSubnet
        {
            self.track_ip_association(overlay, ip);
        }

        ConnectionAdmission::Admitted
    }

    /// Record an overlay sighting from `ip` and act on a cap crossing.
    ///
    /// Detection is score-based, not dial-filter-based: the flagged overlay
    /// is reported through the single scoring path
    /// ([`SwarmScoringEvent::RateLimitExceeded`]) rather than banned
    /// outright, because a shared IPv4 address (CGNAT, campus NAT) can
    /// front many legitimate peers and a direct ban would punish a cohort
    /// for one abuser. Sustained cycling keeps producing fresh flagged
    /// identities; the inbound handshake rate limiter is the enforcement
    /// point that makes that expensive, and it reads the per-IP counts via
    /// [`Self::overlays_seen_from_ip`].
    fn track_ip_association(&self, overlay: OverlayAddress, ip: IpAddr) {
        let now = unix_timestamp_secs();
        let (outcome, tracked) = {
            let mut tracker = self.ip_tracker.lock();
            let outcome = tracker.record(overlay, IpGroup::from(ip), now);
            (outcome, tracker.tracked_ips())
        };
        gauge!("peer_manager_tracked_ips").set(tracked as f64);

        if let RecordOutcome::CyclingDetected { distinct } = outcome {
            counter!("peer_manager_ip_cycling_detections_total").increment(1);
            warn!(
                ?overlay,
                %ip,
                distinct,
                "identity cycling suspected: too many distinct overlays from one IP"
            );
            self.report_peer(
                &overlay,
                SwarmScoringEvent::RateLimitExceeded,
                ReportSource::Topology,
            );
        }
    }

    /// Reserve a live per-IP connection slot for `overlay` arriving from
    /// `ip`, returning `false` when the cap is already reached.
    ///
    /// The decision and the increment happen under one `DashMap` shard
    /// lock so concurrent handshakes from the same IP can never both slip
    /// past the cap. A reconnect of an already-counted overlay is a no-op
    /// that succeeds without double-counting. `max_connections_per_ip` of
    /// `None` admits unconditionally.
    fn try_admit_ip_connection(&self, overlay: OverlayAddress, ip: IpAddr) -> bool {
        let Some(cap) = self.max_connections_per_ip else {
            // Unlimited: still record the group so a future cap could
            // account for it, and so disconnect bookkeeping is symmetric.
            let group = IpGroup::from(ip);
            if self.connection_ip_group.insert(overlay, group).is_none() {
                *self.live_ip_connections.entry(group).or_insert(0) += 1;
                gauge!("peer_manager_max_live_ip_connections")
                    .set(self.live_ip_connections.len() as f64);
            }
            return true;
        };

        let group = IpGroup::from(ip);
        // Already counted (reconnect of a still-tracked overlay): admit
        // without changing the count.
        if self.connection_ip_group.contains_key(&overlay) {
            return true;
        }

        let mut slot = self.live_ip_connections.entry(group).or_insert(0);
        if *slot >= cap {
            return false;
        }
        *slot += 1;
        drop(slot);
        self.connection_ip_group.insert(overlay, group);
        gauge!("peer_manager_max_live_ip_connections").set(self.live_ip_connections.len() as f64);
        true
    }

    /// Decrement the live connection counter for `group`, dropping the
    /// entry when it reaches zero. The caller has already removed (or never
    /// inserted) the overlay's `connection_ip_group` mapping.
    fn release_ip_connection_for_group(&self, group: IpGroup) {
        if let dashmap::mapref::entry::Entry::Occupied(mut e) =
            self.live_ip_connections.entry(group)
        {
            let count = e.get_mut();
            *count = count.saturating_sub(1);
            if *count == 0 {
                e.remove();
            }
        }
        gauge!("peer_manager_max_live_ip_connections").set(self.live_ip_connections.len() as f64);
    }

    /// Live concurrent connections currently counted from `ip`'s group.
    #[must_use]
    pub fn live_connections_from_ip(&self, ip: IpAddr) -> usize {
        self.live_ip_connections
            .get(&IpGroup::from(ip))
            .map_or(0, |r| *r.value())
    }

    /// Distinct overlays seen from `ip`'s tracking group (exact address
    /// for IPv4, /64 prefix for IPv6) within the sighting window.
    ///
    /// Consulted by inbound admission logic such as the handshake rate
    /// limiter to judge whether a source IP is cycling identities before
    /// spending signature recovery on it.
    #[must_use]
    pub fn overlays_seen_from_ip(&self, ip: IpAddr) -> usize {
        self.ip_tracker
            .lock()
            .distinct_overlays(ip, unix_timestamp_secs())
    }

    /// Whether `ip`'s tracking group has shown more distinct overlays
    /// within the window than the configured cap tolerates.
    #[must_use]
    pub fn ip_cycling_suspected(&self, ip: IpAddr) -> bool {
        let mut tracker = self.ip_tracker.lock();
        let cap = tracker.config().max_overlays_per_ip;
        tracker.distinct_overlays(ip, unix_timestamp_secs()) > cap
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
        // Release the live per-IP connection slot reserved at connect time.
        if let Some((_, group)) = self.connection_ip_group.remove(overlay) {
            self.release_ip_connection_for_group(group);
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

    /// Whether a completed handshake has confirmed this peer's identity in
    /// this process.
    ///
    /// Gossip-admitted records start unverified and stay fully dialable;
    /// the first completed handshake on a real connection verifies them
    /// (see [`Self::on_peer_connected`]). The bit is process-local:
    /// snapshot-restored entries start unverified again.
    #[must_use]
    pub fn is_verified(&self, overlay: &OverlayAddress) -> bool {
        self.peers.get(overlay).is_some_and(|e| e.is_verified())
    }

    /// Called by topology when a dial guided by the record for
    /// `dialed_overlay` completed a handshake that asserted a different
    /// overlay: the address belongs to another peer.
    ///
    /// The peer that actually answered is stored and verified through the
    /// normal [`Self::on_peer_connected`] path; this handles the record
    /// that pointed there. An unverified record was a wrong gossip claim
    /// and is removed outright. A once-verified record keeps its history
    /// but takes a dial failure, so backoff and the stale purge retire it
    /// if its addresses now consistently reach someone else.
    pub fn on_dialed_overlay_mismatch(&self, dialed_overlay: &OverlayAddress) {
        let Some(entry) = self.peers.get(dialed_overlay) else {
            return;
        };
        if entry.is_verified() {
            drop(entry);
            debug!(
                ?dialed_overlay,
                "verified peer's address answered as a different overlay; recording dial failure"
            );
            self.record_dial_failure(dialed_overlay);
        } else {
            drop(entry);
            counter!("peer_manager_overlay_mismatch_removed_total").increment(1);
            debug!(
                ?dialed_overlay,
                "removing unverified record: address answered as a different overlay"
            );
            self.remove_peer(dialed_overlay);
        }
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
                // Entries start unverified; the handshake path decrements
                // when it flips the bit.
                gauge!("peer_manager_unverified_peers").increment(1.0);
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

    /// Fully remove a peer from all data structures (index, peer set,
    /// banned set, IP tracker reverse index).
    pub(crate) fn remove_peer(&self, overlay: &OverlayAddress) {
        if let Some((_, entry)) = self.peers.remove(overlay) {
            self.score_distribution.on_peer_removed(entry.score());
            on_health_removed(entry.health_state());
            if !entry.is_verified() {
                gauge!("peer_manager_unverified_peers").decrement(1.0);
            }
        }
        if self.index.remove(overlay) {
            gauge!("peer_manager_total_peers").set(self.index.len() as f64);
        }
        if self.banned_set.remove(overlay).is_some() {
            gauge!("peer_manager_banned_peers").decrement(1.0);
        }
        // Release any live per-IP connection slot still held (a peer removed
        // while connected never reaches on_peer_disconnected for the slot).
        if let Some((_, group)) = self.connection_ip_group.remove(overlay) {
            self.release_ip_connection_for_group(group);
        }
        let tracked = {
            let mut tracker = self.ip_tracker.lock();
            tracker.on_overlay_removed(overlay);
            tracker.tracked_ips()
        };
        gauge!("peer_manager_tracked_ips").set(tracked as f64);
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
            None,
        );
    }

    /// Connect peer `n` asserting it arrived from `ip` with `trust`.
    fn connect_from_ip(pm: &PeerManager<MockIdentity>, n: u8, ip: IpAddr, trust: TrustLevel) {
        pm.on_peer_connected(
            test_swarm_peer(n),
            SwarmNodeType::Client,
            ConnectionDirection::Inbound,
            trust,
            Some(ip),
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
    fn test_gossip_admission_is_unverified_and_dialable() {
        let pm = manager();
        let overlay = test_overlay(1);

        pm.store_discovered_peer(test_swarm_peer(1));
        assert!(!pm.is_verified(&overlay), "gossip admission is unverified");
        assert!(
            pm.eligible_peers().contains(&overlay),
            "unverified peers must be dialable candidates"
        );
        assert_eq!(pm.get_dialable_peers(&[overlay]).len(), 1);
    }

    #[test]
    fn test_handshake_verifies_gossip_admitted_peer() {
        let pm = manager();
        let overlay = test_overlay(1);

        pm.store_discovered_peer(test_swarm_peer(1));
        assert!(!pm.is_verified(&overlay));

        connect(&pm, 1, SwarmNodeType::Storer);
        assert!(
            pm.is_verified(&overlay),
            "the handshake IS the verification"
        );
    }

    #[test]
    fn test_record_update_keeps_peer_verified() {
        let pm = manager();
        let overlay = test_overlay(1);

        connect(&pm, 1, SwarmNodeType::Storer);
        pm.store_discovered_peer(test_swarm_peer(1));
        assert!(
            pm.is_verified(&overlay),
            "a gossiped record refresh must not demote a verified peer"
        );
    }

    #[test]
    fn test_tick_purges_unverified_after_short_failure_budget() {
        let pm = manager();
        pm.store_discovered_peer(test_swarm_peer(1));
        connect(&pm, 2, SwarmNodeType::Storer);
        pm.on_peer_disconnected(&test_overlay(2), "test");

        for n in [1, 2] {
            for _ in 0..3 {
                pm.record_dial_failure(&test_overlay(n));
            }
        }

        pm.tick(unix_timestamp_secs());

        assert!(
            pm.get_swarm_peer(&test_overlay(1)).is_none(),
            "unverified entry purged after three failed dials"
        );
        assert!(
            pm.get_swarm_peer(&test_overlay(2)).is_some(),
            "verified peer keeps the long failure budget"
        );
    }

    #[test]
    fn test_snapshot_skips_unverified_entries() {
        let store = memory_store();
        let pm1 = manager_with_store(Arc::clone(&store));

        connect(&pm1, 1, SwarmNodeType::Storer);
        pm1.store_discovered_peer(test_swarm_peer(2));
        pm1.snapshot();

        let pm2 = manager_with_store(store);
        assert!(
            pm2.get_swarm_peer(&test_overlay(1)).is_some(),
            "verified peers persist"
        );
        assert!(
            pm2.get_swarm_peer(&test_overlay(2)).is_none(),
            "unverified gossip claims never persist"
        );
        assert!(
            !pm2.is_verified(&test_overlay(1)),
            "restored peers re-earn verification on the next handshake"
        );
    }

    #[test]
    fn test_overlay_mismatch_removes_unverified_record() {
        let pm = manager();
        let overlay = test_overlay(1);

        pm.store_discovered_peer(test_swarm_peer(1));
        pm.on_dialed_overlay_mismatch(&overlay);

        assert!(
            pm.get_swarm_peer(&overlay).is_none(),
            "wrong gossip claim removed outright"
        );
        assert!(!pm.index().exists(&overlay));
    }

    #[test]
    fn test_overlay_mismatch_demotes_verified_record() {
        let pm = manager();
        let overlay = test_overlay(1);

        connect(&pm, 1, SwarmNodeType::Storer);
        pm.on_peer_disconnected(&overlay, "test");
        pm.on_dialed_overlay_mismatch(&overlay);

        assert!(
            pm.get_swarm_peer(&overlay).is_some(),
            "once-verified peers keep their record"
        );
        assert!(
            pm.peer_is_in_backoff(&overlay),
            "but take a dial failure so the stale machinery can retire them"
        );
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
    fn test_overlays_in_bin_yield_full_supply() {
        let pm = manager();
        connect(&pm, 1, SwarmNodeType::Storer);

        let bin = Bin::from(test_overlay(0).proximity(&test_overlay(1)));

        let storers: Vec<_> = pm.storer_overlays_in_bin(bin).collect();
        assert_eq!(storers, vec![test_overlay(1)]);

        let dialable: Vec<_> = pm.dialable_overlays_in_bin(bin).collect();
        assert_eq!(dialable, vec![test_overlay(1)]);
    }

    #[test]
    fn test_dialable_overlays_excluding() {
        let pm = manager();
        // Overlays 2 and 3 share a proximity bin relative to the zero local
        // overlay.
        connect(&pm, 2, SwarmNodeType::Storer);
        connect(&pm, 3, SwarmNodeType::Storer);

        let bin = Bin::from(test_overlay(0).proximity(&test_overlay(2)));
        let excluded = test_overlay(2);

        let dialable: Vec<_> = pm
            .dialable_overlays_in_bin_excluding(bin, |o| *o == excluded)
            .collect();
        assert!(!dialable.contains(&excluded));
        assert!(dialable.contains(&test_overlay(3)));
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
    fn test_dialable_in_bin_skips_banned() {
        let pm = manager();

        // First bytes 0x80, 0xc0, 0xa0: all bin 0 relative to the zero local
        // overlay.
        for byte in [0x80, 0xc0, 0xa0] {
            pm.store_discovered_peer(make_swarm_peer_minimal(byte));
        }

        let p1 = OverlayAddress::from(*make_swarm_peer_minimal(0x80).overlay());
        pm.ban(&p1, BanCause::Requested, None);

        let dialable: Vec<_> = pm.dialable_overlays_in_bin(Bin::new(0).unwrap()).collect();
        assert_eq!(dialable.len(), 2);
        assert!(!dialable.contains(&p1));
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
                None,
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
            None,
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
            None,
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
            None,
        );

        pm.store_discovered_peer(test_swarm_peer(1));

        assert_eq!(pm.node_type(&overlay), Some(SwarmNodeType::Storer));
        assert_eq!(pm.trust_level(&overlay), TrustLevel::Trusted);
    }

    /// Manager with a small IP-cycling cap so tests stay compact.
    fn ip_manager(cap: usize) -> Arc<PeerManager<MockIdentity>> {
        PeerManager::new(
            &mock_identity(),
            PeerManagerConfig {
                ip_tracker: IpTrackerConfig {
                    max_overlays_per_ip: cap,
                    window: Duration::from_secs(900),
                    max_sightings_per_ip: cap * 4,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
    }

    const ATTACKER_IP: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 7));

    #[test]
    fn test_ip_cycling_detected_past_cap() {
        let pm = ip_manager(3);

        // A NAT-sized cohort: exactly cap distinct overlays, no penalty.
        for n in 1..=3 {
            connect_from_ip(&pm, n, ATTACKER_IP, TrustLevel::Normal);
        }
        assert!(!pm.ip_cycling_suspected(ATTACKER_IP));
        for n in 1..=3 {
            assert!(pm.get_peer_score(&test_overlay(n)).unwrap() > 0.0);
        }

        // The cap+1th NEW overlay from the same IP is flagged and penalized.
        connect_from_ip(&pm, 4, ATTACKER_IP, TrustLevel::Normal);
        assert!(pm.ip_cycling_suspected(ATTACKER_IP));
        assert_eq!(pm.overlays_seen_from_ip(ATTACKER_IP), 4);
        assert!(
            pm.get_peer_score(&test_overlay(4)).unwrap() < 0.0,
            "the new identity from the over-cap IP must take the penalty"
        );
        // Earlier identities from that IP are not retroactively punished.
        for n in 1..=3 {
            assert!(pm.get_peer_score(&test_overlay(n)).unwrap() > 0.0);
        }
    }

    #[test]
    fn test_ip_reconnect_of_known_overlay_is_not_cycling() {
        let pm = ip_manager(2);

        for n in 1..=2 {
            connect_from_ip(&pm, n, ATTACKER_IP, TrustLevel::Normal);
        }
        // The same peers reconnecting from the same IP must not count as
        // new identities.
        for n in 1..=2 {
            connect_from_ip(&pm, n, ATTACKER_IP, TrustLevel::Normal);
        }
        assert!(!pm.ip_cycling_suspected(ATTACKER_IP));
        assert_eq!(pm.overlays_seen_from_ip(ATTACKER_IP), 2);
        for n in 1..=2 {
            assert!(pm.get_peer_score(&test_overlay(n)).unwrap() > 0.0);
        }
    }

    #[test]
    fn test_ip_tracking_exempts_local_and_trusted_peers() {
        let pm = ip_manager(2);

        // A home LAN: many nodes behind one IP, all local or explicitly
        // trusted. None of them may be recorded, let alone penalized.
        for n in 1..=4 {
            connect_from_ip(&pm, n, ATTACKER_IP, TrustLevel::LocalSubnet);
        }
        for n in 5..=6 {
            connect_from_ip(&pm, n, ATTACKER_IP, TrustLevel::Trusted);
        }

        assert_eq!(pm.overlays_seen_from_ip(ATTACKER_IP), 0);
        assert!(!pm.ip_cycling_suspected(ATTACKER_IP));
        for n in 1..=6 {
            assert!(pm.get_peer_score(&test_overlay(n)).unwrap() > 0.0);
        }
    }

    #[test]
    fn test_ip_reverse_index_cleaned_on_purge() {
        let pm = ip_manager(3);

        for n in 1..=3 {
            connect_from_ip(&pm, n, ATTACKER_IP, TrustLevel::Normal);
        }
        assert_eq!(pm.overlays_seen_from_ip(ATTACKER_IP), 3);

        // Drive peer 2 stale and purge it via the tick path.
        pm.on_peer_disconnected(&test_overlay(2), "test");
        for _ in 0..48 {
            pm.record_dial_failure(&test_overlay(2));
        }
        pm.tick(unix_timestamp_secs());
        assert!(pm.get_swarm_peer(&test_overlay(2)).is_none());

        // The purge freed the IP slot, so a new overlay stays under the cap.
        assert_eq!(pm.overlays_seen_from_ip(ATTACKER_IP), 2);
        connect_from_ip(&pm, 4, ATTACKER_IP, TrustLevel::Normal);
        assert!(!pm.ip_cycling_suspected(ATTACKER_IP));
        assert!(pm.get_peer_score(&test_overlay(4)).unwrap() > 0.0);
    }

    #[test]
    fn test_ip_tracking_groups_ipv6_by_slash64() {
        let pm = ip_manager(2);
        let a: IpAddr = "2001:db8:42:1::1".parse().unwrap();
        let b: IpAddr = "2001:db8:42:1:dead:beef::1".parse().unwrap();
        let other: IpAddr = "2001:db8:42:2::1".parse().unwrap();

        connect_from_ip(&pm, 1, a, TrustLevel::Normal);
        connect_from_ip(&pm, 2, b, TrustLevel::Normal);
        // Third overlay from the same /64 (different interface id) trips
        // the cap.
        connect_from_ip(&pm, 3, a, TrustLevel::Normal);

        assert!(pm.ip_cycling_suspected(a));
        assert!(pm.ip_cycling_suspected(b), "queries group by /64 too");
        assert!(pm.get_peer_score(&test_overlay(3)).unwrap() < 0.0);

        // A neighbouring /64 is unaffected.
        assert!(!pm.ip_cycling_suspected(other));
        connect_from_ip(&pm, 4, other, TrustLevel::Normal);
        assert!(pm.get_peer_score(&test_overlay(4)).unwrap() > 0.0);
    }

    /// Manager with a small live per-IP connection cap.
    fn conn_cap_manager(cap: Option<usize>) -> Arc<PeerManager<MockIdentity>> {
        PeerManager::new(
            &mock_identity(),
            PeerManagerConfig {
                ip_tracker: IpTrackerConfig {
                    max_connections_per_ip: cap,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
    }

    #[test]
    fn test_per_ip_connection_cap_rejects_over_cap() {
        let pm = conn_cap_manager(Some(2));

        // Two connections from one IP are admitted.
        for n in 1..=2 {
            connect_from_ip(&pm, n, ATTACKER_IP, TrustLevel::Normal);
            assert!(pm.is_connected(&test_overlay(n)));
        }
        assert_eq!(pm.live_connections_from_ip(ATTACKER_IP), 2);

        // The third is rejected: never inserted, never connected.
        connect_from_ip(&pm, 3, ATTACKER_IP, TrustLevel::Normal);
        assert!(pm.get_swarm_peer(&test_overlay(3)).is_none());
        assert!(!pm.is_connected(&test_overlay(3)));
        assert_eq!(pm.live_connections_from_ip(ATTACKER_IP), 2);
    }

    #[test]
    fn test_per_ip_connection_cap_returns_rejected_outcome() {
        let pm = conn_cap_manager(Some(1));

        // The first connection from the IP is admitted.
        let admitted = pm.on_peer_connected(
            test_swarm_peer(1),
            SwarmNodeType::Client,
            ConnectionDirection::Inbound,
            TrustLevel::Normal,
            Some(ATTACKER_IP),
        );
        assert_eq!(admitted, ConnectionAdmission::Admitted);
        assert!(admitted.is_admitted());

        // The over-cap connection is reported as rejected so the caller can
        // tear it down instead of treating it as live. Nothing is recorded.
        let rejected = pm.on_peer_connected(
            test_swarm_peer(2),
            SwarmNodeType::Client,
            ConnectionDirection::Inbound,
            TrustLevel::Normal,
            Some(ATTACKER_IP),
        );
        assert_eq!(rejected, ConnectionAdmission::RejectedIpCap);
        assert!(!rejected.is_admitted());
        assert!(!pm.is_connected(&test_overlay(2)));
        assert!(pm.get_swarm_peer(&test_overlay(2)).is_none());
    }

    #[test]
    fn test_per_ip_connection_cap_freed_on_disconnect() {
        let pm = conn_cap_manager(Some(2));

        for n in 1..=2 {
            connect_from_ip(&pm, n, ATTACKER_IP, TrustLevel::Normal);
        }
        // Disconnecting one frees a slot for a new connection.
        pm.on_peer_disconnected(&test_overlay(1), "test");
        assert_eq!(pm.live_connections_from_ip(ATTACKER_IP), 1);

        connect_from_ip(&pm, 3, ATTACKER_IP, TrustLevel::Normal);
        assert!(pm.is_connected(&test_overlay(3)));
        assert_eq!(pm.live_connections_from_ip(ATTACKER_IP), 2);
    }

    #[test]
    fn test_per_ip_connection_cap_exempts_local_subnet() {
        let pm = conn_cap_manager(Some(1));

        // A home LAN behind one IP: every node is admitted despite the cap.
        for n in 1..=5 {
            connect_from_ip(&pm, n, ATTACKER_IP, TrustLevel::LocalSubnet);
            assert!(pm.is_connected(&test_overlay(n)));
        }
        // Exempt peers are not counted at all.
        assert_eq!(pm.live_connections_from_ip(ATTACKER_IP), 0);
    }

    #[test]
    fn test_per_ip_connection_cap_unlimited_when_none() {
        let pm = conn_cap_manager(None);

        for n in 1..=10 {
            connect_from_ip(&pm, n, ATTACKER_IP, TrustLevel::Normal);
            assert!(pm.is_connected(&test_overlay(n)));
        }
    }

    #[test]
    fn test_per_ip_connection_cap_reconnect_not_double_counted() {
        let pm = conn_cap_manager(Some(2));

        connect_from_ip(&pm, 1, ATTACKER_IP, TrustLevel::Normal);
        // Same overlay reconnecting must not consume a second slot.
        connect_from_ip(&pm, 1, ATTACKER_IP, TrustLevel::Normal);
        assert_eq!(pm.live_connections_from_ip(ATTACKER_IP), 1);

        // The second distinct overlay still fits under the cap.
        connect_from_ip(&pm, 2, ATTACKER_IP, TrustLevel::Normal);
        assert_eq!(pm.live_connections_from_ip(ATTACKER_IP), 2);
    }

    #[test]
    fn test_tick_decays_disconnected_score_across_ticks() {
        let pm = manager();
        let overlay = test_overlay(1);
        pm.store_discovered_peer(test_swarm_peer(1));
        for _ in 0..4 {
            pm.report_peer(
                &overlay,
                SwarmScoringEvent::ProtocolError,
                ReportSource::Topology,
            );
        }
        assert_eq!(pm.get_peer_score(&overlay), Some(-12.0));

        // Two ticks spanning one disconnected half-life (10 minutes).
        let start = unix_timestamp_secs();
        pm.tick(start + 300);
        pm.tick(start + 600);

        let score = pm.get_peer_score(&overlay).unwrap();
        assert!(
            (score + 6.0).abs() < 0.1,
            "one disconnected half-life must halve the score, got {score}"
        );
    }

    #[test]
    fn test_tick_decays_connected_score_at_double_rate() {
        let pm = manager();
        let overlay = test_overlay(1);
        connect(&pm, 1, SwarmNodeType::Client); // ConnectionSuccess: +1
        for _ in 0..4 {
            pm.report_peer(
                &overlay,
                SwarmScoringEvent::ProtocolError,
                ReportSource::Topology,
            );
        }
        assert_eq!(pm.get_peer_score(&overlay), Some(-11.0));

        // 10 minutes is two connected half-lives: the score quarters.
        let start = unix_timestamp_secs();
        pm.tick(start + 600);

        let score = pm.get_peer_score(&overlay).unwrap();
        assert!(
            (score + 2.75).abs() < 0.1,
            "two connected half-lives must quarter the score, got {score}"
        );
    }

    #[test]
    fn test_decay_is_robust_to_missed_ticks() {
        let pm = manager();
        let overlay = test_overlay(1);
        pm.store_discovered_peer(test_swarm_peer(1));
        for _ in 0..4 {
            pm.report_peer(
                &overlay,
                SwarmScoringEvent::ProtocolError,
                ReportSource::Topology,
            );
        }

        // A single late tick spanning two half-lives decays by the true
        // elapsed time, not by one tick interval.
        let start = unix_timestamp_secs();
        pm.tick(start + 1200);

        let score = pm.get_peer_score(&overlay).unwrap();
        assert!(
            (score + 3.0).abs() < 0.1,
            "a missed tick must still decay by the full elapsed time, got {score}"
        );
    }

    #[test]
    fn test_positive_scores_decay_too() {
        let pm = manager();
        let overlay = test_overlay(1);
        connect(&pm, 1, SwarmNodeType::Client); // +1

        let start = unix_timestamp_secs();
        pm.tick(start + 300); // one connected half-life

        let score = pm.get_peer_score(&overlay).unwrap();
        assert!(
            (score - 0.5).abs() < 0.05,
            "positive reputation is recency-weighted and decays, got {score}"
        );
    }

    #[test]
    fn test_ban_expiry_resets_score_and_emits_unbanned_once() {
        let pm = manager();
        let overlay = test_overlay(1);
        connect(&pm, 1, SwarmNodeType::Client);
        let mut rx = pm.subscribe();

        pm.ban(&overlay, BanCause::Requested, Some("test".into()));
        let until = drain_events(&mut rx)
            .iter()
            .find_map(|e| match e {
                PeerLifecycleEvent::Banned {
                    overlay: o, until, ..
                } if *o == overlay => *until,
                _ => None,
            })
            .expect("timed ban must carry an expiry");

        // One second before expiry: still banned.
        pm.tick(until - 1);
        assert!(pm.is_banned(&overlay));
        assert!(
            drain_events(&mut rx)
                .iter()
                .all(|e| !matches!(e, PeerLifecycleEvent::Unbanned { .. }))
        );

        // Exactly at expiry (now >= until): unbanned, score reset to the
        // disconnect threshold so the peer must behave to climb back.
        pm.tick(until);
        assert!(!pm.is_banned(&overlay));
        assert_eq!(pm.banned_count(), 0);
        assert!(pm.eligible_peers().contains(&overlay));
        assert_eq!(
            pm.get_peer_score(&overlay),
            Some(pm.scoring_config.disconnect_threshold()),
            "an expired ban resets the score to the disconnect threshold"
        );
        let unbans = drain_events(&mut rx)
            .iter()
            .filter(|e| matches!(e, PeerLifecycleEvent::Unbanned { overlay: o } if *o == overlay))
            .count();
        assert_eq!(unbans, 1, "Unbanned must be emitted exactly once");

        // Later ticks do not re-emit.
        pm.tick(until + 600);
        assert!(
            drain_events(&mut rx)
                .iter()
                .all(|e| !matches!(e, PeerLifecycleEvent::Unbanned { .. }))
        );
    }

    #[test]
    fn test_permanent_ban_never_expires() {
        let pm = manager();
        let overlay = test_overlay(1);
        connect(&pm, 1, SwarmNodeType::Client);
        let mut rx = pm.subscribe();

        pm.ban_permanent(&overlay, BanCause::Requested, Some("operator".into()));
        assert!(drain_events(&mut rx).iter().any(|e| matches!(
            e,
            PeerLifecycleEvent::Banned {
                overlay: o,
                until: None,
                ..
            } if *o == overlay
        )));

        let start = unix_timestamp_secs();
        pm.tick(start + 365 * 24 * 3600);
        assert!(pm.is_banned(&overlay), "permanent bans never expire");
        assert!(
            drain_events(&mut rx)
                .iter()
                .all(|e| !matches!(e, PeerLifecycleEvent::Unbanned { .. }))
        );
    }

    #[test]
    fn test_reports_for_banned_peer_are_dropped() {
        let pm = thresholds_manager();
        let overlay = test_overlay(1);
        connect(&pm, 1, SwarmNodeType::Client);
        let mut rx = pm.subscribe();

        // Drive the peer to the ban threshold.
        for _ in 0..15 {
            pm.report_peer(
                &overlay,
                SwarmScoringEvent::ProtocolError,
                ReportSource::Protocol("test"),
            );
        }
        assert!(pm.is_banned(&overlay));
        let until = drain_events(&mut rx)
            .iter()
            .find_map(|e| match e {
                PeerLifecycleEvent::Banned {
                    overlay: o, until, ..
                } if *o == overlay => *until,
                _ => None,
            })
            .expect("auto-ban must carry an expiry");
        let score_at_ban = pm.get_peer_score(&overlay).unwrap();

        // Lingering streams keep reporting: every report is dropped.
        for _ in 0..5 {
            pm.report_peer(
                &overlay,
                SwarmScoringEvent::ProtocolError,
                ReportSource::Protocol("test"),
            );
        }
        assert_eq!(
            pm.get_peer_score(&overlay),
            Some(score_at_ban),
            "reports while banned must not change the score"
        );
        assert!(
            drain_events(&mut rx).is_empty(),
            "reports while banned must not emit lifecycle events"
        );

        // The original expiry stands: the extra reports did not extend it.
        pm.tick(until);
        assert!(
            !pm.is_banned(&overlay),
            "reports during the ban must not extend the expiry"
        );
    }

    #[test]
    fn test_warning_fires_again_after_decay_recovery() {
        let pm = thresholds_manager();
        let overlay = test_overlay(1);
        connect(&pm, 1, SwarmNodeType::Client); // +1
        let mut rx = pm.subscribe();

        // +1 -> -11: crosses the warn threshold (-10) once.
        for _ in 0..4 {
            pm.report_peer(
                &overlay,
                SwarmScoringEvent::ProtocolError,
                ReportSource::Topology,
            );
        }
        let warnings = drain_events(&mut rx)
            .iter()
            .filter(|e| matches!(e, PeerLifecycleEvent::ScoreWarning { .. }))
            .count();
        assert_eq!(warnings, 1);

        // One connected half-life halves -11 to about -5.5, above the warn
        // threshold: the one-shot warning re-arms.
        let start = unix_timestamp_secs();
        pm.tick(start + 300);
        assert!(pm.get_peer_score(&overlay).unwrap() > -10.0);

        // Descend again: about -8.5 then about -11.5, warning fires again.
        for _ in 0..2 {
            pm.report_peer(
                &overlay,
                SwarmScoringEvent::ProtocolError,
                ReportSource::Topology,
            );
        }
        let warnings = drain_events(&mut rx)
            .iter()
            .filter(|e| matches!(e, PeerLifecycleEvent::ScoreWarning { .. }))
            .count();
        assert_eq!(warnings, 1, "a recovered peer must be warnable again");
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
