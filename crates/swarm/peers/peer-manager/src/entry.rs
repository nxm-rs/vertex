//! Per-peer state with lock-free scoring and persistence types.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use metrics::gauge;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use vertex_net_local::IpCapability;
use vertex_net_peer_backoff::PeerBackoff;
use vertex_net_peer_store::NetRecord;
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_peer_score::{
    PeerScore, ScoreCallbacks, SwarmPeerScore, SwarmScoringConfig, SwarmScoringEvent,
};
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

/// `(banned_at_unix_secs, reason)`.
pub type BanInfo = (u64, String);

/// Persistence record for a Swarm peer.
///
/// Score data is stored separately in `ScoreTable` to allow lazy loading.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredPeer {
    pub peer: SwarmPeer,
    pub node_type: SwarmNodeType,
    pub ban_info: Option<BanInfo>,
    pub first_seen: u64,
    pub last_seen: u64,
    pub last_dial_attempt: u64,
    pub consecutive_failures: u32,
}

impl StoredPeer {
    /// Create a default record for a newly discovered peer (via gossip).
    pub fn new_discovered(peer: SwarmPeer) -> Self {
        let now = unix_timestamp_secs();
        Self {
            peer,
            node_type: SwarmNodeType::Client,
            ban_info: None,
            first_seen: now,
            last_seen: now,
            last_dial_attempt: 0,
            consecutive_failures: 0,
        }
    }

    pub fn is_banned(&self) -> bool {
        self.ban_info.is_some()
    }

    /// Check if the stored peer is dialable (not banned and not in backoff).
    pub fn is_dialable(&self) -> bool {
        if self.ban_info.is_some() {
            return false;
        }
        if self.consecutive_failures == 0 {
            return true;
        }
        if self.last_dial_attempt == 0 {
            return true;
        }
        let backoff =
            PeerBackoff::from_persisted(self.last_dial_attempt, self.consecutive_failures);
        let overlay = OverlayAddress::from(*self.peer.overlay());
        let jitter_seed = jitter_seed_from_overlay(&overlay);
        backoff
            .remaining_jittered(unix_timestamp_secs(), jitter_seed)
            .is_none()
    }
}

impl NetRecord for StoredPeer {
    type Id = OverlayAddress;
    fn id(&self) -> &OverlayAddress {
        self.peer.overlay()
    }
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

pub(crate) struct PeerEntry {
    identity: RwLock<(SwarmPeer, SwarmNodeType)>,
    scoring: SwarmPeerScore,
    first_seen: AtomicU64,
    last_seen: AtomicU64,
    backoff: PeerBackoff,
    ban_info: RwLock<Option<BanInfo>>,
    jitter_seed: u64,
    dirty: AtomicBool,
}

impl PeerEntry {
    pub(crate) fn with_config(
        peer: SwarmPeer,
        node_type: SwarmNodeType,
        overlay: OverlayAddress,
        config: Arc<SwarmScoringConfig>,
        callbacks: Arc<ScoreCallbacks>,
    ) -> Self {
        let now = unix_timestamp_secs();
        Self {
            identity: RwLock::new((peer, node_type)),
            scoring: SwarmPeerScore::new(overlay, PeerScore::new(), config, callbacks),
            first_seen: AtomicU64::new(now),
            last_seen: AtomicU64::new(now),
            backoff: PeerBackoff::new(),
            ban_info: RwLock::new(None),
            jitter_seed: jitter_seed_from_overlay(&overlay),
            dirty: AtomicBool::new(true),
        }
    }

    pub(crate) fn from_record(
        record: StoredPeer,
        scoring: Option<PeerScore>,
        config: Arc<SwarmScoringConfig>,
        callbacks: Arc<ScoreCallbacks>,
    ) -> Self {
        let overlay = OverlayAddress::from(*record.peer.overlay());
        Self {
            identity: RwLock::new((record.peer, record.node_type)),
            scoring: SwarmPeerScore::new(overlay, scoring.unwrap_or_default(), config, callbacks),
            first_seen: AtomicU64::new(record.first_seen),
            last_seen: AtomicU64::new(record.last_seen),
            backoff: PeerBackoff::from_persisted(
                record.last_dial_attempt,
                record.consecutive_failures,
            ),
            ban_info: RwLock::new(record.ban_info),
            jitter_seed: jitter_seed_from_overlay(&overlay),
            dirty: AtomicBool::new(false),
        }
    }

    pub(crate) fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Release);
    }

    /// Atomically clears the dirty flag and returns its previous value.
    pub(crate) fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }

    pub(crate) fn swarm_peer(&self) -> SwarmPeer {
        self.identity.read().0.clone()
    }

    pub(crate) fn ip_capability(&self) -> IpCapability {
        self.identity.read().0.ip_capability()
    }

    pub(crate) fn node_type(&self) -> SwarmNodeType {
        self.identity.read().1
    }

    pub(crate) fn score(&self) -> f64 {
        self.scoring.score()
    }

    pub(crate) fn first_seen(&self) -> u64 {
        self.first_seen.load(Ordering::Relaxed)
    }

    pub(crate) fn last_seen(&self) -> u64 {
        self.last_seen.load(Ordering::Relaxed)
    }

    pub(crate) fn consecutive_failures(&self) -> u32 {
        self.backoff.consecutive_failures()
    }

    /// Update the peer identity and node type.
    ///
    /// Only refreshes `last_seen` if the peer has no active failures.
    /// This prevents gossip re-verification from keeping permanently unreachable
    /// peers alive — only successful connections (`record_success`) should reset
    /// the staleness clock for failed peers.
    pub(crate) fn update_peer(&self, peer: SwarmPeer, node_type: SwarmNodeType) {
        let mut guard = self.identity.write();
        guard.0 = peer;
        guard.1 = node_type;
        drop(guard);
        if self.consecutive_failures() == 0 {
            self.touch();
        }
        self.mark_dirty();
    }

    /// Update peer addresses without changing the node type.
    ///
    /// Used by gossip discovery to refresh multiaddrs for already-known peers
    /// without overwriting the handshake-confirmed node type.
    pub(crate) fn update_addresses(&self, peer: SwarmPeer) {
        let mut guard = self.identity.write();
        guard.0 = peer;
        drop(guard);
        if self.consecutive_failures() == 0 {
            self.touch();
        }
        self.mark_dirty();
    }

    pub(crate) fn record_event(&self, event: SwarmScoringEvent) {
        self.scoring.record_event(event);
        self.mark_dirty();
    }

    pub(crate) fn record_success(&self, latency: Duration) {
        self.scoring.record_connection_success(Some(latency));
        self.reset_failures();
        self.touch();
        self.mark_dirty();
    }

    pub(crate) fn set_latency(&self, rtt: Duration) {
        self.scoring.set_latency(rtt);
        self.mark_dirty();
    }

    pub(crate) fn ban(&self, reason: Option<String>) {
        *self.ban_info.write() = Some((unix_timestamp_secs(), reason.unwrap_or_default()));
        self.mark_dirty();
    }

    pub(crate) fn record_dial_failure(&self) {
        self.backoff.record_failure(unix_timestamp_secs());
        self.mark_dirty();
    }

    /// Re-increments `consecutive_failures` so backoff applies even though
    /// `record_success()` already reset it during handshake.
    pub(crate) fn record_early_disconnect(&self, duration: Duration) {
        self.scoring.record_early_disconnect(duration);
        self.backoff.record_failure(unix_timestamp_secs());
        self.mark_dirty();
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

impl From<&PeerEntry> for StoredPeer {
    fn from(entry: &PeerEntry) -> Self {
        let (ref peer, node_type) = *entry.identity.read();
        Self {
            peer: peer.clone(),
            node_type,
            ban_info: entry.ban_info.read().clone(),
            first_seen: entry.first_seen(),
            last_seen: entry.last_seen(),
            last_dial_attempt: entry.backoff.last_attempt(),
            consecutive_failures: entry.consecutive_failures(),
        }
    }
}

impl PeerEntry {
    pub(crate) fn score_snapshot(&self) -> PeerScore {
        self.scoring.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_peer_score::ScoreCallbacks;
    use vertex_swarm_test_utils::test_swarm_peer;

    fn test_entry(n: u8, node_type: SwarmNodeType) -> PeerEntry {
        let peer = test_swarm_peer(n);
        let overlay = OverlayAddress::from(*peer.overlay());
        PeerEntry::with_config(
            peer,
            node_type,
            overlay,
            Arc::new(SwarmScoringConfig::default()),
            ScoreCallbacks::noop(),
        )
    }

    #[test]
    fn test_new_entry() {
        let entry = test_entry(1, SwarmNodeType::Storer);
        assert_eq!(entry.score(), 0.0);
        assert!(!entry.is_banned());
        assert!(entry.is_dialable());
        assert_eq!(entry.node_type(), SwarmNodeType::Storer);
        assert!(entry.first_seen() > 0);
    }

    #[test]
    fn test_scoring_on_success() {
        let entry = test_entry(1, SwarmNodeType::Client);
        entry.record_success(Duration::from_millis(50));
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
    fn test_record_roundtrip() {
        let entry = test_entry(1, SwarmNodeType::Storer);
        entry.record_success(Duration::from_millis(100));

        let record = StoredPeer::from(&entry);
        let score = entry.score_snapshot();
        let restored = PeerEntry::from_record(
            record,
            Some(score),
            Arc::new(SwarmScoringConfig::default()),
            ScoreCallbacks::noop(),
        );

        assert!((restored.score() - entry.score()).abs() < 0.01);
        assert_eq!(restored.node_type(), entry.node_type());
    }

    #[test]
    fn test_ban_record_roundtrip() {
        let entry = test_entry(1, SwarmNodeType::Client);
        entry.ban(Some("test reason".to_string()));

        let record = StoredPeer::from(&entry);
        assert!(record.is_banned());

        let restored = PeerEntry::from_record(
            record,
            None,
            Arc::new(SwarmScoringConfig::default()),
            ScoreCallbacks::noop(),
        );
        assert!(restored.is_banned());
        assert!(!restored.is_dialable());
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

        entry.record_success(Duration::from_millis(50));
        assert_eq!(entry.consecutive_failures(), 0);
        assert!(entry.is_dialable());
    }

    #[test]
    fn test_backoff_record_roundtrip() {
        let entry = test_entry(1, SwarmNodeType::Client);
        entry.record_dial_failure();
        entry.record_dial_failure();

        let record = StoredPeer::from(&entry);
        assert_eq!(record.consecutive_failures, 2);
        assert!(record.last_dial_attempt > 0);

        let restored = PeerEntry::from_record(
            record,
            None,
            Arc::new(SwarmScoringConfig::default()),
            ScoreCallbacks::noop(),
        );
        assert_eq!(restored.consecutive_failures(), 2);
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
    fn test_serialization_roundtrip() {
        let peer = test_swarm_peer(1);
        let record = StoredPeer {
            peer,
            node_type: SwarmNodeType::Storer,
            ban_info: Some((100, "test".into())),
            first_seen: 100,
            last_seen: 200,
            last_dial_attempt: 150,
            consecutive_failures: 3,
        };
        let bytes = postcard::to_allocvec(&record).unwrap();
        let restored: StoredPeer = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(restored.node_type, SwarmNodeType::Storer);
        assert_eq!(restored.first_seen, 100);
        assert!(restored.ban_info.is_some());
    }
}
