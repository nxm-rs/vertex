//! Arc-per-peer state for lock-free hot paths.

use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::RwLock;
use vertex_net_local::IpCapability;
use vertex_net_peer_score::PeerScore;
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_peer_score::SwarmScoringConfig;
use vertex_swarm_primitives::SwarmNodeType;

use crate::ban::BanInfo;
use crate::snapshot::SwarmPeerSnapshot;

/// Base backoff duration in seconds (30 seconds).
const BASE_BACKOFF_SECS: u64 = 30;
/// Maximum backoff duration in seconds (1 hour).
const MAX_BACKOFF_SECS: u64 = 3600;
/// Stale peer threshold in seconds (1 week).
const STALE_THRESHOLD_SECS: u64 = 7 * 24 * 3600;

/// Per-peer state with lock-free scoring and atomic timestamps.
pub struct PeerEntry {
    peer: RwLock<SwarmPeer>,
    node_type: SwarmNodeType,
    score: Arc<PeerScore>,
    config: Arc<SwarmScoringConfig>,
    first_seen: AtomicU64,
    last_seen: AtomicU64,
    /// Unix timestamp of last dial attempt.
    last_dial_attempt: AtomicU64,
    /// Consecutive dial failures (reset on success).
    consecutive_failures: AtomicU32,
    /// Ban information (None = not banned).
    ban_info: RwLock<Option<BanInfo>>,
}

impl PeerEntry {
    /// Create a new entry with scoring config.
    pub fn with_config(
        peer: SwarmPeer,
        node_type: SwarmNodeType,
        config: Arc<SwarmScoringConfig>,
    ) -> Self {
        let now = unix_timestamp_secs();
        Self {
            peer: RwLock::new(peer),
            node_type,
            score: Arc::new(PeerScore::new()),
            config,
            first_seen: AtomicU64::new(now),
            last_seen: AtomicU64::new(now),
            last_dial_attempt: AtomicU64::new(0),
            consecutive_failures: AtomicU32::new(0),
            ban_info: RwLock::new(None),
        }
    }

    /// Create from a persistence snapshot with scoring config.
    pub fn from_snapshot_with_config(
        snapshot: SwarmPeerSnapshot,
        config: Arc<SwarmScoringConfig>,
    ) -> Self {
        let score = Arc::new(PeerScore::new());
        score.restore(&snapshot.scoring);

        Self {
            peer: RwLock::new(snapshot.peer),
            node_type: snapshot.node_type,
            score,
            config,
            first_seen: AtomicU64::new(snapshot.first_seen),
            last_seen: AtomicU64::new(snapshot.last_seen),
            last_dial_attempt: AtomicU64::new(snapshot.last_dial_attempt),
            consecutive_failures: AtomicU32::new(snapshot.consecutive_failures),
            ban_info: RwLock::new(snapshot.ban_info),
        }
    }

    pub fn swarm_peer(&self) -> SwarmPeer {
        self.peer.read().clone()
    }

    pub fn ip_capability(&self) -> IpCapability {
        self.peer.read().ip_capability()
    }

    pub fn node_type(&self) -> SwarmNodeType {
        self.node_type
    }

    /// Update the peer identity (multiaddrs may change).
    pub fn update_peer(&self, peer: SwarmPeer) {
        *self.peer.write() = peer;
        self.touch();
    }

    pub fn score(&self) -> f64 {
        self.score.score()
    }

    /// Record a successful connection with latency.
    pub fn record_success(&self, latency: Duration) {
        self.score.record_success(latency.as_nanos() as u64);
        self.score.add_score(self.config.connection_success());
        self.reset_failures();
        self.touch();
    }

    /// Set latency sample without affecting score.
    pub fn set_latency(&self, rtt: Duration) {
        self.score.record_latency(rtt.as_nanos() as u64);
    }

    pub fn latency(&self) -> Option<Duration> {
        self.score.avg_latency()
    }

    pub fn first_seen(&self) -> u64 {
        self.first_seen.load(Ordering::Relaxed)
    }

    pub fn last_seen(&self) -> u64 {
        self.last_seen.load(Ordering::Relaxed)
    }

    /// Update last_seen to current time.
    pub fn touch(&self) {
        self.last_seen.store(unix_timestamp_secs(), Ordering::Relaxed);
    }

    pub fn is_banned(&self) -> bool {
        self.ban_info.read().is_some()
    }

    /// Ban the peer.
    pub fn ban(&self, reason: Option<String>) {
        *self.ban_info.write() = Some(BanInfo::new(reason));
    }

    /// Create a persistence snapshot.
    pub fn snapshot(&self) -> SwarmPeerSnapshot {
        SwarmPeerSnapshot {
            peer: self.peer.read().clone(),
            node_type: self.node_type,
            scoring: self.score.snapshot(),
            ban_info: self.ban_info.read().clone(),
            first_seen: self.first_seen(),
            last_seen: self.last_seen(),
            last_dial_attempt: self.last_dial_attempt(),
            consecutive_failures: self.consecutive_failures(),
        }
    }

    pub fn record_dial_attempt(&self) {
        self.last_dial_attempt.store(unix_timestamp_secs(), Ordering::Relaxed);
    }

    pub fn record_dial_failure(&self) {
        self.consecutive_failures.fetch_add(1, Ordering::Relaxed);
        self.record_dial_attempt();
    }

    /// Reset consecutive failures (called on successful connection).
    pub fn reset_failures(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
    }

    pub fn last_dial_attempt(&self) -> u64 {
        self.last_dial_attempt.load(Ordering::Relaxed)
    }

    /// Returns None if never attempted, Some(timestamp) otherwise.
    pub fn last_dial_attempt_time(&self) -> Option<NonZeroU64> {
        NonZeroU64::new(self.last_dial_attempt.load(Ordering::Relaxed))
    }

    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures.load(Ordering::Relaxed)
    }

    /// Calculate backoff duration based on consecutive failures.
    /// Returns None if no backoff needed (no failures or backoff expired).
    pub fn backoff_remaining(&self) -> Option<Duration> {
        let failures = self.consecutive_failures();
        if failures == 0 {
            return None;
        }

        let last_attempt = self.last_dial_attempt_time()?;

        // Exponential backoff: base * 2^(failures-1), capped at max
        let backoff_secs = BASE_BACKOFF_SECS
            .saturating_mul(1u64 << (failures - 1).min(10))
            .min(MAX_BACKOFF_SECS);

        let now = unix_timestamp_secs();
        let backoff_until = last_attempt.get().saturating_add(backoff_secs);

        if now >= backoff_until {
            None
        } else {
            Some(Duration::from_secs(backoff_until - now))
        }
    }

    pub fn is_in_backoff(&self) -> bool {
        self.backoff_remaining().is_some()
    }

    /// Check if peer is stale (no successful connection in threshold period).
    /// Only considers peers stale if they have failures and haven't connected recently.
    pub fn is_stale(&self) -> bool {
        let failures = self.consecutive_failures();
        if failures == 0 {
            return false;
        }

        let last_seen = self.last_seen();
        let now = unix_timestamp_secs();

        now.saturating_sub(last_seen) > STALE_THRESHOLD_SECS
    }
}

fn unix_timestamp_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_test_utils::test_swarm_peer;

    fn test_entry(n: u8, node_type: SwarmNodeType) -> PeerEntry {
        let peer = test_swarm_peer(n);
        PeerEntry::with_config(peer, node_type, Arc::new(SwarmScoringConfig::default()))
    }

    #[test]
    fn test_new_entry() {
        let entry = test_entry(1, SwarmNodeType::Storer);

        assert_eq!(entry.score(), 0.0);
        assert!(!entry.is_banned());
        assert_eq!(entry.node_type(), SwarmNodeType::Storer);
        assert!(entry.first_seen() > 0);
    }

    #[test]
    fn test_scoring_on_success() {
        let entry = test_entry(1, SwarmNodeType::Client);

        entry.record_success(Duration::from_millis(50));
        assert!(entry.score() > 0.0);
        assert!(entry.latency().is_some());
    }

    #[test]
    fn test_custom_config() {
        let peer = test_swarm_peer(1);
        let config = Arc::new(SwarmScoringConfig::lenient());
        let entry = PeerEntry::with_config(peer, SwarmNodeType::Client, config);

        // Just verify entry was created with custom config
        assert_eq!(entry.node_type(), SwarmNodeType::Client);
    }

    #[test]
    fn test_ban() {
        let entry = test_entry(1, SwarmNodeType::Client);

        entry.ban(Some("test".to_string()));
        assert!(entry.is_banned());
    }

    #[test]
    fn test_snapshot_roundtrip() {
        let entry = test_entry(1, SwarmNodeType::Storer);
        entry.record_success(Duration::from_millis(100));

        let snapshot = entry.snapshot();
        let config = Arc::new(SwarmScoringConfig::default());
        let restored = PeerEntry::from_snapshot_with_config(snapshot, config);

        assert!((restored.score() - entry.score()).abs() < 0.01);
        assert_eq!(restored.node_type(), entry.node_type());
    }

    #[test]
    fn test_ban_snapshot_roundtrip() {
        let entry = test_entry(1, SwarmNodeType::Client);
        entry.ban(Some("test reason".to_string()));
        assert!(entry.is_banned());

        let snapshot = entry.snapshot();
        assert!(snapshot.ban_info.is_some());

        let config = Arc::new(SwarmScoringConfig::default());
        let restored = PeerEntry::from_snapshot_with_config(snapshot, config);
        assert!(restored.is_banned());
    }

    #[test]
    fn test_dial_failure_backoff() {
        let entry = test_entry(1, SwarmNodeType::Client);

        assert!(!entry.is_in_backoff());
        assert_eq!(entry.consecutive_failures(), 0);

        entry.record_dial_failure();
        assert_eq!(entry.consecutive_failures(), 1);
        assert!(entry.is_in_backoff());
        let backoff = entry.backoff_remaining().unwrap();
        assert!(backoff.as_secs() <= 30);

        entry.record_dial_failure();
        assert_eq!(entry.consecutive_failures(), 2);
        let backoff = entry.backoff_remaining().unwrap();
        assert!(backoff.as_secs() <= 60);
    }

    #[test]
    fn test_success_resets_failures() {
        let entry = test_entry(1, SwarmNodeType::Client);

        entry.record_dial_failure();
        entry.record_dial_failure();
        entry.record_dial_failure();
        assert_eq!(entry.consecutive_failures(), 3);

        entry.record_success(Duration::from_millis(50));
        assert_eq!(entry.consecutive_failures(), 0);
        assert!(!entry.is_in_backoff());
    }

    #[test]
    fn test_backoff_snapshot_roundtrip() {
        let entry = test_entry(1, SwarmNodeType::Client);

        entry.record_dial_failure();
        entry.record_dial_failure();

        let snapshot = entry.snapshot();
        assert_eq!(snapshot.consecutive_failures, 2);
        assert!(snapshot.last_dial_attempt > 0);

        let config = Arc::new(SwarmScoringConfig::default());
        let restored = PeerEntry::from_snapshot_with_config(snapshot, config);
        assert_eq!(restored.consecutive_failures(), 2);
    }

    #[test]
    fn test_ip_capability_computed() {
        let entry = test_entry(1, SwarmNodeType::Storer);
        let cap = entry.ip_capability();
        assert!(!cap.is_empty());
    }

    #[test]
    fn test_node_type_variants() {
        let bootnode = test_entry(1, SwarmNodeType::Bootnode);
        assert_eq!(bootnode.node_type(), SwarmNodeType::Bootnode);

        let client = test_entry(2, SwarmNodeType::Client);
        assert_eq!(client.node_type(), SwarmNodeType::Client);

        let storer = test_entry(3, SwarmNodeType::Storer);
        assert_eq!(storer.node_type(), SwarmNodeType::Storer);
    }
}
