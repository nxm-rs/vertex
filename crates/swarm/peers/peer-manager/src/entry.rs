//! Per-peer state with lock-free scoring and the snapshot record.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use metrics::gauge;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tracing::debug;
use vertex_net_local::IpCapability;
use vertex_net_peer_backoff::PeerBackoff;
use vertex_net_peer_registry::ConnectionDirection;
use vertex_swarm_api::SwarmScoringEvent;
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_peer_score::{PeerScore, ScoreChange, SwarmPeerScore, SwarmScoringConfig};
use vertex_swarm_primitives::{OverlayAddress, SwarmNodeType};

/// Exclusive health state for a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HealthState {
    Healthy,
    Failing,
    Stale,
    Banned,
}

impl HealthState {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Failing => "failing",
            Self::Stale => "stale",
            Self::Banned => "banned",
        }
    }
}

pub(crate) fn on_health_added(state: HealthState) {
    gauge!("peer_manager_health", "state" => state.label()).increment(1.0);
}

pub(crate) fn on_health_removed(state: HealthState) {
    gauge!("peer_manager_health", "state" => state.label()).decrement(1.0);
}

pub(crate) fn on_health_changed(old: HealthState, new: HealthState) {
    if old != new {
        gauge!("peer_manager_health", "state" => old.label()).decrement(1.0);
        gauge!("peer_manager_health", "state" => new.label()).increment(1.0);
    }
}

/// Stale if no successful connection in this period (24 hours).
const STALE_THRESHOLD_SECS: u64 = 24 * 3600;

/// Stale regardless of last_seen after this many consecutive failures (~48h of persistent failure).
const STALE_FAILURE_THRESHOLD: u32 = 48;

/// `(banned_at_unix_secs, reason)`. Runtime-only; never persisted.
pub(crate) type BanInfo = (u64, String);

/// Identity-only persistence record for a Swarm peer.
///
/// Deliberately slim: bans, dial backoff, and scores are runtime state that
/// is re-earned within seconds of reconnecting, so none of it is persisted.
/// A snapshot carries just enough to redial the peer after a restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerSnapshot {
    /// The signed peer record (overlay, multiaddrs, handshake signature).
    pub peer: SwarmPeer,
    /// Last known node type; provisional until the next handshake confirms it.
    pub node_type: SwarmNodeType,
    /// Unix seconds the peer was last seen healthy.
    pub last_seen: u64,
}

pub(crate) fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn jitter_seed_from_overlay(overlay: &OverlayAddress) -> u64 {
    // OverlayAddress is B256 (32 bytes); first 8 bytes always exist.
    let b = &overlay.0;
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

/// How much the local node trusts a peer when ranking it for eviction.
///
/// Computed once at handshake completion by topology (which owns the listen
/// addresses and the dial reason) and stored on the peer entry, so eviction
/// ranking reads one atomic instead of re-deriving address scope per trim
/// round. The value is process-local and refreshed on every connect; gossip
/// never writes it.
///
/// Ordered from least to most protected: `Normal < LocalSubnet < Trusted`.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    strum::Display,
    strum::IntoStaticStr,
    strum::FromRepr,
)]
#[strum(serialize_all = "snake_case")]
#[repr(u8)]
pub enum TrustLevel {
    /// No special standing; ranked purely by reachability.
    #[default]
    Normal = 0,
    /// Loopback, link-local, or same-subnet peer; protected from
    /// capacity-driven trimming when local-peer trust is enabled.
    LocalSubnet = 1,
    /// Explicitly configured peer (static/trusted multiaddrs); never evicted
    /// by bin trimming.
    Trusted = 2,
}

/// A [`SwarmNodeType`] plus a confirmed bit, packed into one atomic byte.
///
/// A peer's node type flows in from two sources with different trust levels:
/// hive gossip (provisional, unverified) and the handshake (asserted by the
/// peer itself). Gossip may set or refresh the provisional value only while
/// the cell is unconfirmed. A completed handshake confirms the value; from
/// then on provisional writes are ignored. A later handshake may re-confirm
/// a different value, since a node can legitimately change its type between
/// sessions (for example upgrading from client to storer).
///
/// The confirmed bit is process-local: records restored from a snapshot
/// start unconfirmed until the next handshake.
#[derive(Debug)]
pub(crate) struct NodeTypeCell(AtomicU8);

/// High bit marks the node type as handshake-confirmed; the low bits hold
/// the [`SwarmNodeType`] discriminant.
const CONFIRMED_BIT: u8 = 0x80;

impl NodeTypeCell {
    pub(crate) fn provisional(node_type: SwarmNodeType) -> Self {
        Self(AtomicU8::new(node_type as u8))
    }

    pub(crate) fn get(&self) -> SwarmNodeType {
        SwarmNodeType::from_repr(self.0.load(Ordering::Acquire) & !CONFIRMED_BIT)
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn is_confirmed(&self) -> bool {
        self.0.load(Ordering::Acquire) & CONFIRMED_BIT != 0
    }

    /// Set the provisional value (gossip path).
    ///
    /// Returns `false` and leaves the cell untouched once the value has been
    /// handshake-confirmed.
    pub(crate) fn set_provisional(&self, node_type: SwarmNodeType) -> bool {
        self.0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current & CONFIRMED_BIT == 0).then_some(node_type as u8)
            })
            .is_ok()
    }

    /// Store the asserted value and mark the cell confirmed (handshake path).
    pub(crate) fn confirm(&self, node_type: SwarmNodeType) {
        self.0
            .store(node_type as u8 | CONFIRMED_BIT, Ordering::Release);
    }
}

/// Encoding of `Option<ConnectionDirection>` in one atomic byte.
const DIRECTION_NONE: u8 = 0;
const DIRECTION_OUTBOUND: u8 = 1;
const DIRECTION_INBOUND: u8 = 2;

fn direction_to_repr(direction: ConnectionDirection) -> u8 {
    match direction {
        ConnectionDirection::Outbound => DIRECTION_OUTBOUND,
        ConnectionDirection::Inbound => DIRECTION_INBOUND,
    }
}

fn direction_from_repr(repr: u8) -> Option<ConnectionDirection> {
    match repr {
        DIRECTION_OUTBOUND => Some(ConnectionDirection::Outbound),
        DIRECTION_INBOUND => Some(ConnectionDirection::Inbound),
        _ => None,
    }
}

pub(crate) struct PeerEntry {
    peer: RwLock<SwarmPeer>,
    node_type: NodeTypeCell,
    scoring: SwarmPeerScore,
    last_seen: AtomicU64,
    backoff: PeerBackoff,
    ban_info: RwLock<Option<BanInfo>>,
    jitter_seed: u64,
    /// Unix seconds since the current connection completed its handshake;
    /// 0 while disconnected. Process-local, never persisted.
    connected_since: AtomicU64,
    /// Direction of the current connection ([`DIRECTION_NONE`] while
    /// disconnected). Process-local, never persisted.
    direction: AtomicU8,
    /// [`TrustLevel`] discriminant, written at handshake completion only.
    trust: AtomicU8,
}

impl PeerEntry {
    pub(crate) fn with_config(
        peer: SwarmPeer,
        node_type: SwarmNodeType,
        overlay: OverlayAddress,
        config: Arc<SwarmScoringConfig>,
    ) -> Self {
        let now = unix_timestamp_secs();
        Self {
            peer: RwLock::new(peer),
            node_type: NodeTypeCell::provisional(node_type),
            scoring: SwarmPeerScore::new(PeerScore::new(), config),
            last_seen: AtomicU64::new(now),
            backoff: PeerBackoff::new(),
            ban_info: RwLock::new(None),
            jitter_seed: jitter_seed_from_overlay(&overlay),
            connected_since: AtomicU64::new(0),
            direction: AtomicU8::new(DIRECTION_NONE),
            trust: AtomicU8::new(TrustLevel::Normal as u8),
        }
    }

    /// Rebuild an entry from a persisted snapshot.
    ///
    /// Only identity survives a restart: the score starts at zero, the
    /// backoff is clear, and the node type is provisional until the next
    /// handshake confirms it.
    pub(crate) fn from_snapshot(snapshot: PeerSnapshot, config: Arc<SwarmScoringConfig>) -> Self {
        let overlay = OverlayAddress::from(*snapshot.peer.overlay());
        Self {
            node_type: NodeTypeCell::provisional(snapshot.node_type),
            peer: RwLock::new(snapshot.peer),
            scoring: SwarmPeerScore::new(PeerScore::new(), config),
            last_seen: AtomicU64::new(snapshot.last_seen),
            backoff: PeerBackoff::new(),
            ban_info: RwLock::new(None),
            jitter_seed: jitter_seed_from_overlay(&overlay),
            connected_since: AtomicU64::new(0),
            direction: AtomicU8::new(DIRECTION_NONE),
            trust: AtomicU8::new(TrustLevel::Normal as u8),
        }
    }

    pub(crate) fn swarm_peer(&self) -> SwarmPeer {
        self.peer.read().clone()
    }

    /// Signed wall-clock timestamp of the currently held peer record.
    pub(crate) fn timestamp(&self) -> vertex_swarm_peer::Timestamp {
        self.peer.read().timestamp()
    }

    pub(crate) fn ip_capability(&self) -> IpCapability {
        self.peer.read().ip_capability()
    }

    pub(crate) fn node_type(&self) -> SwarmNodeType {
        self.node_type.get()
    }

    pub(crate) fn score(&self) -> f64 {
        self.scoring.score()
    }

    pub(crate) fn last_seen(&self) -> u64 {
        self.last_seen.load(Ordering::Relaxed)
    }

    pub(crate) fn consecutive_failures(&self) -> u32 {
        self.backoff.consecutive_failures()
    }

    /// Update peer addresses without changing the node type.
    ///
    /// Used by gossip discovery to refresh multiaddrs for already-known peers
    /// without overwriting the handshake-confirmed node type.
    ///
    /// Only refreshes `last_seen` if the peer has no active failures.
    /// This prevents gossip re-verification from keeping permanently
    /// unreachable peers alive; only successful connections
    /// (via `set_connected`) should reset the staleness clock for failed peers.
    pub(crate) fn update_addresses(&self, peer: SwarmPeer) {
        *self.peer.write() = peer;
        if self.consecutive_failures() == 0 {
            self.touch();
        }
    }

    /// Refresh the provisional node type (gossip path).
    ///
    /// Ignored once a handshake has confirmed the node type; a differing
    /// proposal is debug-logged and dropped.
    pub(crate) fn set_provisional_node_type(&self, node_type: SwarmNodeType) {
        if !self.node_type.set_provisional(node_type) && self.node_type.get() != node_type {
            debug!(
                proposed = %node_type,
                confirmed = %self.node_type.get(),
                "ignoring provisional node type for handshake-confirmed peer"
            );
        }
    }

    /// Confirm the peer-asserted node type (handshake path).
    ///
    /// May re-confirm a different value on reconnect: a node can change its
    /// type between sessions. Gossip can never change a confirmed value.
    pub(crate) fn confirm_node_type(&self, node_type: SwarmNodeType) {
        self.node_type.confirm(node_type);
    }

    /// Apply a scoring event, returning the outcome and score transition.
    ///
    /// Score changes flow exclusively through the peer manager's report
    /// path; this is the only entry-level scoring write.
    pub(crate) fn record_event(&self, event: SwarmScoringEvent) -> ScoreChange {
        self.scoring.record_event_change(event)
    }

    /// Record connection state at handshake completion.
    ///
    /// Stores connected-since, direction, and the topology-computed
    /// [`TrustLevel`], resets the failure backoff, and refreshes `last_seen`.
    /// Scoring is not touched here; the manager reports the connection
    /// success through its report path.
    pub(crate) fn set_connected(&self, direction: ConnectionDirection, trust: TrustLevel) {
        self.connected_since
            .store(unix_timestamp_secs(), Ordering::Release);
        self.direction
            .store(direction_to_repr(direction), Ordering::Release);
        self.trust.store(trust as u8, Ordering::Release);
        self.reset_failures();
        self.touch();
    }

    /// Clear connection state when the last connection to the peer closes.
    ///
    /// The stored [`TrustLevel`] is kept: it describes the peer, not the
    /// connection, and the next handshake recomputes it.
    pub(crate) fn clear_connected(&self) {
        self.connected_since.store(0, Ordering::Release);
        self.direction.store(DIRECTION_NONE, Ordering::Release);
    }

    pub(crate) fn is_connected(&self) -> bool {
        self.connected_since.load(Ordering::Acquire) != 0
    }

    /// Unix seconds at which the current connection completed its handshake,
    /// or `None` while disconnected.
    pub(crate) fn connected_since(&self) -> Option<u64> {
        match self.connected_since.load(Ordering::Acquire) {
            0 => None,
            since => Some(since),
        }
    }

    pub(crate) fn direction(&self) -> Option<ConnectionDirection> {
        direction_from_repr(self.direction.load(Ordering::Acquire))
    }

    pub(crate) fn trust_level(&self) -> TrustLevel {
        TrustLevel::from_repr(self.trust.load(Ordering::Acquire)).unwrap_or_default()
    }

    pub(crate) fn record_latency(&self, rtt: Duration) {
        self.scoring.record_latency(rtt);
    }

    pub(crate) fn ban(&self, reason: Option<String>) {
        *self.ban_info.write() = Some((unix_timestamp_secs(), reason.unwrap_or_default()));
    }

    pub(crate) fn record_dial_failure(&self) {
        self.backoff.record_failure(unix_timestamp_secs());
    }

    pub(crate) fn is_banned(&self) -> bool {
        self.ban_info.read().is_some()
    }

    pub(crate) fn is_dialable(&self) -> bool {
        !self.is_banned() && !self.is_in_backoff()
    }

    /// Backoff with per-peer jitter (+/-25%) to prevent synchronized retry storms.
    pub(crate) fn backoff_remaining(&self) -> Option<Duration> {
        self.backoff
            .remaining_jittered(unix_timestamp_secs(), self.jitter_seed)
    }

    pub(crate) fn is_in_backoff(&self) -> bool {
        self.backoff_remaining().is_some()
    }

    pub(crate) fn is_stale(&self) -> bool {
        let failures = self.consecutive_failures();
        if failures == 0 {
            return false;
        }
        if failures >= STALE_FAILURE_THRESHOLD {
            return true;
        }
        unix_timestamp_secs().saturating_sub(self.last_seen()) > STALE_THRESHOLD_SECS
    }

    pub(crate) fn health_state(&self) -> HealthState {
        if self.is_banned() {
            return HealthState::Banned;
        }
        if self.is_stale() {
            return HealthState::Stale;
        }
        if self.consecutive_failures() > 0 {
            return HealthState::Failing;
        }
        HealthState::Healthy
    }

    fn touch(&self) {
        self.last_seen
            .store(unix_timestamp_secs(), Ordering::Relaxed);
    }

    fn reset_failures(&self) {
        self.backoff.reset();
    }
}

impl From<&PeerEntry> for PeerSnapshot {
    fn from(entry: &PeerEntry) -> Self {
        Self {
            peer: entry.peer.read().clone(),
            node_type: entry.node_type.get(),
            last_seen: entry.last_seen(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_test_utils::test_swarm_peer;

    fn test_entry(n: u8, node_type: SwarmNodeType) -> PeerEntry {
        let peer = test_swarm_peer(n);
        let overlay = OverlayAddress::from(*peer.overlay());
        PeerEntry::with_config(
            peer,
            node_type,
            overlay,
            Arc::new(SwarmScoringConfig::default()),
        )
    }

    /// Connection-success scoring plus the bookkeeping `set_connected` does,
    /// mirroring what `PeerManager::on_peer_connected` performs.
    fn record_success(entry: &PeerEntry, latency: Duration) {
        entry.set_connected(ConnectionDirection::Outbound, TrustLevel::Normal);
        entry.record_event(SwarmScoringEvent::ConnectionSuccess {
            latency: Some(latency),
        });
    }

    #[test]
    fn test_new_entry() {
        let entry = test_entry(1, SwarmNodeType::Storer);
        assert_eq!(entry.score(), 0.0);
        assert!(!entry.is_banned());
        assert!(entry.is_dialable());
        assert_eq!(entry.node_type(), SwarmNodeType::Storer);
        assert!(entry.last_seen() > 0);
    }

    #[test]
    fn test_scoring_on_success() {
        let entry = test_entry(1, SwarmNodeType::Client);
        record_success(&entry, Duration::from_millis(50));
        assert!(entry.score() > 0.0);
    }

    #[test]
    fn test_ban() {
        let entry = test_entry(1, SwarmNodeType::Client);
        assert!(entry.is_dialable());

        entry.ban(Some("test".to_string()));
        assert!(entry.is_banned());
        assert!(!entry.is_dialable());
    }

    #[test]
    fn test_snapshot_roundtrip_resets_runtime_state() {
        let entry = test_entry(1, SwarmNodeType::Storer);
        record_success(&entry, Duration::from_millis(100));
        entry.ban(Some("test reason".to_string()));
        entry.record_dial_failure();

        let snapshot = PeerSnapshot::from(&entry);
        assert_eq!(snapshot.node_type, SwarmNodeType::Storer);

        let restored = PeerEntry::from_snapshot(snapshot, Arc::new(SwarmScoringConfig::default()));
        assert_eq!(restored.node_type(), SwarmNodeType::Storer);
        // Reputation, bans, and backoff never survive a restart.
        assert_eq!(restored.score(), 0.0);
        assert!(!restored.is_banned());
        assert_eq!(restored.consecutive_failures(), 0);
        assert!(restored.is_dialable());
    }

    #[test]
    fn test_dial_failure_backoff() {
        let entry = test_entry(1, SwarmNodeType::Client);
        assert!(!entry.is_in_backoff());

        entry.record_dial_failure();
        assert_eq!(entry.consecutive_failures(), 1);
        assert!(entry.is_in_backoff());
        assert!(!entry.is_dialable());
        assert!(entry.backoff_remaining().unwrap().as_secs() <= 38);

        entry.record_dial_failure();
        assert_eq!(entry.consecutive_failures(), 2);
        assert!(entry.backoff_remaining().unwrap().as_secs() <= 76);
    }

    #[test]
    fn test_success_resets_failures() {
        let entry = test_entry(1, SwarmNodeType::Client);
        for _ in 0..3 {
            entry.record_dial_failure();
        }
        assert_eq!(entry.consecutive_failures(), 3);

        record_success(&entry, Duration::from_millis(50));
        assert_eq!(entry.consecutive_failures(), 0);
        assert!(entry.is_dialable());
    }

    #[test]
    fn test_node_type_variants() {
        for (n, nt) in [
            (1, SwarmNodeType::Bootnode),
            (2, SwarmNodeType::Client),
            (3, SwarmNodeType::Storer),
        ] {
            assert_eq!(test_entry(n, nt).node_type(), nt);
        }
    }

    #[test]
    fn test_update_addresses_preserves_node_type() {
        let entry = test_entry(1, SwarmNodeType::Storer);
        assert_eq!(entry.node_type(), SwarmNodeType::Storer);

        // Update addresses with a different SwarmPeer (same overlay, different addrs)
        let new_peer = test_swarm_peer(1);
        entry.update_addresses(new_peer);

        // Node type must remain Storer
        assert_eq!(entry.node_type(), SwarmNodeType::Storer);
    }

    #[test]
    fn test_node_type_cell_gossip_then_handshake() {
        let cell = NodeTypeCell::provisional(SwarmNodeType::Client);
        assert!(!cell.is_confirmed());
        assert_eq!(cell.get(), SwarmNodeType::Client);

        // Gossip may refresh the provisional value while unconfirmed.
        assert!(cell.set_provisional(SwarmNodeType::Bootnode));
        assert_eq!(cell.get(), SwarmNodeType::Bootnode);
        assert!(!cell.is_confirmed());

        // The handshake confirms the asserted value.
        cell.confirm(SwarmNodeType::Storer);
        assert!(cell.is_confirmed());
        assert_eq!(cell.get(), SwarmNodeType::Storer);
    }

    #[test]
    fn test_node_type_cell_gossip_ignored_after_confirm() {
        let cell = NodeTypeCell::provisional(SwarmNodeType::Client);
        cell.confirm(SwarmNodeType::Storer);

        assert!(!cell.set_provisional(SwarmNodeType::Client));
        assert_eq!(cell.get(), SwarmNodeType::Storer);
        assert!(cell.is_confirmed());
    }

    #[test]
    fn test_node_type_cell_reconfirm_on_reconnect() {
        let cell = NodeTypeCell::provisional(SwarmNodeType::Client);
        cell.confirm(SwarmNodeType::Client);

        // The peer reconnects asserting a different type: the handshake path
        // re-confirms (a node can upgrade from client to storer between
        // sessions).
        cell.confirm(SwarmNodeType::Storer);
        assert_eq!(cell.get(), SwarmNodeType::Storer);

        // Gossip still cannot change the confirmed value.
        assert!(!cell.set_provisional(SwarmNodeType::Client));
        assert_eq!(cell.get(), SwarmNodeType::Storer);
    }

    #[test]
    fn test_entry_provisional_node_type_ignored_after_confirm() {
        let entry = test_entry(1, SwarmNodeType::Client);

        // Unconfirmed: gossip may refresh the provisional value.
        entry.set_provisional_node_type(SwarmNodeType::Bootnode);
        assert_eq!(entry.node_type(), SwarmNodeType::Bootnode);

        // Handshake confirms; later gossip proposals are dropped.
        entry.confirm_node_type(SwarmNodeType::Storer);
        entry.set_provisional_node_type(SwarmNodeType::Client);
        assert_eq!(entry.node_type(), SwarmNodeType::Storer);

        // A reconnect handshake may still re-confirm a different type.
        entry.confirm_node_type(SwarmNodeType::Client);
        assert_eq!(entry.node_type(), SwarmNodeType::Client);
    }

    #[test]
    fn test_serialization_roundtrip() {
        let peer = test_swarm_peer(1);
        let record = PeerSnapshot {
            peer,
            node_type: SwarmNodeType::Storer,
            last_seen: 200,
        };
        let bytes = postcard::to_allocvec(&record).unwrap();
        let restored: PeerSnapshot = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(restored.node_type, SwarmNodeType::Storer);
        assert_eq!(restored.last_seen, 200);
    }
}
