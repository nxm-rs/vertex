//! Arc-per-peer state for lock-free hot paths, plus persistence types.

use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use vertex_net_local::IpCapability;
use std::time::{SystemTime, UNIX_EPOCH};

use vertex_net_peer_store::{BackoffState, DEFAULT_BASE_BACKOFF_SECS, DEFAULT_MAX_BACKOFF_SECS, NetRecord};
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_peer_score::{PeerScore, PeerScoreSnapshot, ScoreObserver, SwarmPeerScore, SwarmScoringConfig, SwarmScoringEvent};
use vertex_swarm_primitives::{OverlayAddress, SwarmNodeType};

use crate::health::HealthState;

/// Ban metadata: `(banned_at_unix_secs, reason)`.
pub type BanInfo = (u64, String);

/// Flat persistence record for a Swarm peer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredPeer {
    pub peer: SwarmPeer,
    pub node_type: SwarmNodeType,
    pub scoring: PeerScoreSnapshot,
    pub ban_info: Option<BanInfo>,
    pub first_seen: u64,
    pub last_seen: u64,
    pub last_dial_attempt: u64,
    pub consecutive_failures: u32,
}

impl StoredPeer {
    pub fn is_banned(&self) -> bool {
        self.ban_info.is_some()
    }
}

impl NetRecord for StoredPeer {
    type Id = OverlayAddress;
    fn id(&self) -> &OverlayAddress { self.peer.overlay() }
}

/// Stale peer threshold in seconds (24 hours).
///
/// A peer is stale if it has failures AND hasn't been successfully connected
/// in this period. Gossip re-verification does not refresh the staleness clock.
const STALE_THRESHOLD_SECS: u64 = 24 * 3600;

/// A peer with this many consecutive failures is considered stale regardless
/// of when it was last seen. With 1-hour max backoff, 48 failures ~ 48 hours
/// of persistent failure.
const STALE_FAILURE_THRESHOLD: u32 = 48;

/// Per-peer state with lock-free scoring and atomic timestamps.
pub(crate) struct PeerEntry {
    identity: RwLock<(SwarmPeer, SwarmNodeType)>,
    scoring: SwarmPeerScore,
    first_seen: AtomicU64,
    last_seen: AtomicU64,
    last_dial_attempt: AtomicU64,
    consecutive_failures: AtomicU32,
    ban_info: RwLock<Option<BanInfo>>,
    jitter_seed: u64,
}

impl PeerEntry {
    pub(crate) fn with_config(
        peer: SwarmPeer,
        node_type: SwarmNodeType,
        overlay: OverlayAddress,
        config: Arc<SwarmScoringConfig>,
        observer: Arc<dyn ScoreObserver>,
    ) -> Self {
        let now = unix_timestamp_secs();
        Self {
            identity: RwLock::new((peer, node_type)),
            scoring: SwarmPeerScore::new(overlay, PeerScore::new(), config, observer),
            first_seen: AtomicU64::new(now),
            last_seen: AtomicU64::new(now),
            last_dial_attempt: AtomicU64::new(0),
            consecutive_failures: AtomicU32::new(0),
            ban_info: RwLock::new(None),
            jitter_seed: jitter_seed_from_overlay(&overlay),
        }
    }

    pub(crate) fn from_record(
        record: StoredPeer,
        config: Arc<SwarmScoringConfig>,
        observer: Arc<dyn ScoreObserver>,
    ) -> Self {
        let overlay = OverlayAddress::from(*record.peer.overlay());
        Self {
            identity: RwLock::new((record.peer, record.node_type)),
            scoring: SwarmPeerScore::new(
                overlay,
                PeerScore::from(&record.scoring),
                config,
                observer,
            ),
            first_seen: AtomicU64::new(record.first_seen),
            last_seen: AtomicU64::new(record.last_seen),
            last_dial_attempt: AtomicU64::new(record.last_dial_attempt),
            consecutive_failures: AtomicU32::new(record.consecutive_failures),
            ban_info: RwLock::new(record.ban_info),
            jitter_seed: jitter_seed_from_overlay(&overlay),
        }
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
    }

    pub(crate) fn score(&self) -> f64 {
        self.scoring.score()
    }

    pub(crate) fn record_event(&self, event: SwarmScoringEvent) {
        self.scoring.record_event(event);
    }

    pub(crate) fn record_success(&self, latency: Duration) {
        self.scoring.record_connection_success(Some(latency));
        self.reset_failures();
        self.touch();
    }

    pub(crate) fn set_latency(&self, rtt: Duration) {
        self.scoring.set_latency(rtt);
    }

    pub(crate) fn first_seen(&self) -> u64 {
        self.first_seen.load(Ordering::Relaxed)
    }

    pub(crate) fn last_seen(&self) -> u64 {
        self.last_seen.load(Ordering::Relaxed)
    }

    pub(crate) fn is_banned(&self) -> bool {
        self.ban_info.read().is_some()
    }

    pub(crate) fn ban(&self, reason: Option<String>) {
        *self.ban_info.write() = Some((unix_timestamp_secs(), reason.unwrap_or_default()));
    }

    /// Not banned and not in backoff.
    pub(crate) fn is_dialable(&self) -> bool {
        !self.is_banned() && !self.is_in_backoff()
    }

    pub(crate) fn record_dial_failure(&self) {
        self.consecutive_failures.fetch_add(1, Ordering::Relaxed);
        self.record_dial_attempt();
    }

    /// Record an early disconnect (post-handshake connection that failed quickly).
    ///
    /// Applies a scoring penalty and re-increments `consecutive_failures` so
    /// backoff applies even though `record_success()` already reset it to 0
    /// during handshake.
    pub(crate) fn record_early_disconnect(&self, duration: Duration) {
        self.scoring.record_early_disconnect(duration);
        self.consecutive_failures.fetch_add(1, Ordering::Relaxed);
        self.record_dial_attempt();
    }

    pub(crate) fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures.load(Ordering::Relaxed)
    }

    /// Calculate backoff duration based on consecutive failures.
    /// Uses per-peer jitter (+/-25%) to prevent synchronized retry storms.
    pub(crate) fn backoff_remaining(&self) -> Option<Duration> {
        let failures = self.consecutive_failures();
        if failures == 0 {
            return None;
        }
        let last_attempt = self.last_dial_attempt_time()?;
        BackoffState::new(last_attempt.get(), failures).remaining_jittered(
            unix_timestamp_secs(),
            DEFAULT_BASE_BACKOFF_SECS,
            DEFAULT_MAX_BACKOFF_SECS,
            self.jitter_seed,
        )
    }

    pub(crate) fn is_in_backoff(&self) -> bool {
        self.backoff_remaining().is_some()
    }

    /// A peer is stale if it has failures and either 48+ consecutive failures
    /// or hasn't been successfully connected in 24 hours.
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

    /// Compute the current health state from atomic fields.
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
        self.last_seen.store(unix_timestamp_secs(), Ordering::Relaxed);
    }

    fn reset_failures(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
    }

    fn record_dial_attempt(&self) {
        self.last_dial_attempt.store(unix_timestamp_secs(), Ordering::Relaxed);
    }

    fn last_dial_attempt(&self) -> u64 {
        self.last_dial_attempt.load(Ordering::Relaxed)
    }

    fn last_dial_attempt_time(&self) -> Option<NonZeroU64> {
        NonZeroU64::new(self.last_dial_attempt.load(Ordering::Relaxed))
    }
}

impl From<&PeerEntry> for StoredPeer {
    fn from(entry: &PeerEntry) -> Self {
        let (ref peer, node_type) = *entry.identity.read();
        Self {
            peer: peer.clone(),
            node_type,
            scoring: entry.scoring.snapshot(),
            ban_info: entry.ban_info.read().clone(),
            first_seen: entry.first_seen(),
            last_seen: entry.last_seen(),
            last_dial_attempt: entry.last_dial_attempt(),
            consecutive_failures: entry.consecutive_failures(),
        }
    }
}

fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn jitter_seed_from_overlay(overlay: &OverlayAddress) -> u64 {
    let bytes = overlay.0.as_slice();
    u64::from_le_bytes(bytes[..8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_peer_score::NoOpScoreObserver;
    use vertex_swarm_test_utils::test_swarm_peer;

    fn test_entry(n: u8, node_type: SwarmNodeType) -> PeerEntry {
        let peer = test_swarm_peer(n);
        let overlay = OverlayAddress::from(*peer.overlay());
        PeerEntry::with_config(
            peer, node_type, overlay,
            Arc::new(SwarmScoringConfig::default()),
            Arc::new(NoOpScoreObserver),
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
        let restored = PeerEntry::from_record(
            record, Arc::new(SwarmScoringConfig::default()), Arc::new(NoOpScoreObserver),
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
            record, Arc::new(SwarmScoringConfig::default()), Arc::new(NoOpScoreObserver),
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
        for _ in 0..3 { entry.record_dial_failure(); }
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
            record, Arc::new(SwarmScoringConfig::default()), Arc::new(NoOpScoreObserver),
        );
        assert_eq!(restored.consecutive_failures(), 2);
    }

    #[test]
    fn test_node_type_variants() {
        for (n, nt) in [(1, SwarmNodeType::Bootnode), (2, SwarmNodeType::Client), (3, SwarmNodeType::Storer)] {
            assert_eq!(test_entry(n, nt).node_type(), nt);
        }
    }

    #[test]
    fn test_serialization_roundtrip() {
        let peer = test_swarm_peer(1);
        let record = StoredPeer {
            peer,
            node_type: SwarmNodeType::Storer,
            scoring: PeerScoreSnapshot::default(),
            ban_info: Some((100, "test".into())),
            first_seen: 100,
            last_seen: 200,
            last_dial_attempt: 150,
            consecutive_failures: 3,
        };
        let json = serde_json::to_string(&record).unwrap();
        let restored: StoredPeer = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.node_type, SwarmNodeType::Storer);
        assert_eq!(restored.first_seen, 100);
        assert!(restored.ban_info.is_some());
    }
}
