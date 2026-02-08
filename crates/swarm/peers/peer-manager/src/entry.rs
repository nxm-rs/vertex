//! Arc-per-peer state for lock-free hot paths.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::RwLock;
use vertex_net_peer_score::PeerScore;
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_peer_score::SwarmScoringConfig;

use crate::ban::BanInfo;
use crate::data::SwarmPeerData;
use crate::snapshot::SwarmPeerSnapshot;
use crate::IpCapability;

const ORD: Ordering = Ordering::Relaxed;

/// Per-peer state with lock-free scoring and atomic timestamps.
pub struct PeerEntry {
    data: RwLock<SwarmPeerData>,
    score: Arc<PeerScore>,
    config: Arc<SwarmScoringConfig>,
    first_seen: AtomicU64,
    last_seen: AtomicU64,
    ban_info: RwLock<Option<BanInfo>>,
}

impl PeerEntry {
    /// Create a new entry from SwarmPeerData with default scoring config.
    pub fn new(data: SwarmPeerData) -> Self {
        Self::with_config(data, Arc::new(SwarmScoringConfig::default()))
    }

    /// Create a new entry with custom scoring config.
    pub fn with_config(data: SwarmPeerData, config: Arc<SwarmScoringConfig>) -> Self {
        let now = unix_timestamp_secs();
        Self {
            data: RwLock::new(data),
            score: Arc::new(PeerScore::new()),
            config,
            first_seen: AtomicU64::new(now),
            last_seen: AtomicU64::new(now),
            ban_info: RwLock::new(None),
        }
    }

    /// Create from a persistence snapshot.
    pub fn from_snapshot(snapshot: SwarmPeerSnapshot) -> Self {
        Self::from_snapshot_with_config(snapshot, Arc::new(SwarmScoringConfig::default()))
    }

    /// Create from a persistence snapshot with custom scoring config.
    pub fn from_snapshot_with_config(
        snapshot: SwarmPeerSnapshot,
        config: Arc<SwarmScoringConfig>,
    ) -> Self {
        let data = SwarmPeerData::new(snapshot.peer.clone(), snapshot.full_node);

        let score = Arc::new(PeerScore::new());
        score.restore(&snapshot.scoring);

        Self {
            data: RwLock::new(data),
            score,
            config,
            first_seen: AtomicU64::new(snapshot.first_seen),
            last_seen: AtomicU64::new(snapshot.last_seen),
            ban_info: RwLock::new(snapshot.ban_info),
        }
    }

    /// Get read-only access to data.
    pub fn data(&self) -> parking_lot::RwLockReadGuard<'_, SwarmPeerData> {
        self.data.read()
    }

    /// Get cloned SwarmPeer.
    pub fn swarm_peer(&self) -> SwarmPeer {
        self.data.read().swarm_peer().clone()
    }

    /// Get IP capability.
    pub fn ip_capability(&self) -> IpCapability {
        self.data.read().ip_capability()
    }

    /// Check if this peer is a full node.
    pub fn is_full_node(&self) -> bool {
        self.data.read().is_full_node()
    }

    /// Update the peer data.
    pub fn update_data(&self, data: SwarmPeerData) {
        *self.data.write() = data;
        self.touch();
    }

    /// Get current score.
    pub fn score(&self) -> f64 {
        self.score.score()
    }

    /// Get a clone of the scoring Arc.
    pub fn scoring(&self) -> &Arc<PeerScore> {
        &self.score
    }

    /// Add to score.
    pub fn add_score(&self, delta: f64) {
        self.score.add_score(delta);
    }

    /// Record a successful connection with latency.
    pub fn record_success(&self, latency: Duration) {
        self.score.record_success(latency.as_nanos() as u64);
        self.score.add_score(self.config.connection_success);
        self.touch();
    }

    /// Record a connection timeout.
    pub fn record_timeout(&self) {
        self.score.record_timeout();
        self.score.add_score(self.config.connection_timeout);
    }

    /// Record a connection refusal.
    pub fn record_refusal(&self) {
        self.score.record_refusal();
        self.score.add_score(self.config.connection_refused);
    }

    /// Record a handshake failure.
    pub fn record_handshake_failure(&self) {
        self.score.record_handshake_failure();
        self.score.add_score(self.config.handshake_failure);
    }

    /// Record a protocol error.
    pub fn record_protocol_error(&self) {
        self.score.record_protocol_error();
        self.score.add_score(self.config.protocol_error);
    }

    /// Set latency sample without affecting score.
    pub fn set_latency(&self, rtt: Duration) {
        self.score.record_latency(rtt.as_nanos() as u64);
    }

    /// Get average latency if available.
    pub fn latency(&self) -> Option<Duration> {
        self.score.avg_latency()
    }

    /// Check if peer should be banned based on score.
    pub fn should_ban(&self) -> bool {
        self.config.should_ban(self.score.score())
    }

    /// Unix timestamp when peer was first seen.
    pub fn first_seen(&self) -> u64 {
        self.first_seen.load(ORD)
    }

    /// Unix timestamp when peer was last seen.
    pub fn last_seen(&self) -> u64 {
        self.last_seen.load(ORD)
    }

    /// Update last_seen to current time.
    pub fn touch(&self) {
        self.last_seen.store(unix_timestamp_secs(), ORD);
    }

    /// Check if peer is banned.
    pub fn is_banned(&self) -> bool {
        self.ban_info.read().is_some()
    }

    /// Get ban info if banned.
    pub fn ban_info(&self) -> Option<BanInfo> {
        self.ban_info.read().clone()
    }

    /// Ban the peer.
    pub fn ban(&self, reason: Option<String>) {
        *self.ban_info.write() = Some(BanInfo::new(reason));
    }

    /// Unban the peer.
    pub fn unban(&self) {
        *self.ban_info.write() = None;
    }

    /// Create a persistence snapshot.
    pub fn snapshot(&self) -> SwarmPeerSnapshot {
        let data = self.data.read();
        SwarmPeerSnapshot {
            peer: data.swarm_peer().clone(),
            ip_capability: data.ip_capability(),
            full_node: data.is_full_node(),
            scoring: self.score.snapshot(),
            ban_info: self.ban_info.read().clone(),
            first_seen: self.first_seen(),
            last_seen: self.last_seen(),
        }
    }
}

impl std::fmt::Debug for PeerEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerEntry")
            .field("score", &self.score())
            .field("first_seen", &self.first_seen())
            .field("last_seen", &self.last_seen())
            .field("is_banned", &self.is_banned())
            .finish_non_exhaustive()
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

    #[test]
    fn test_new_entry() {
        let peer = test_swarm_peer(1);
        let data = SwarmPeerData::new(peer.clone(), true);
        let entry = PeerEntry::new(data);

        assert_eq!(entry.score(), 0.0);
        assert!(!entry.is_banned());
        assert!(entry.is_full_node());
        assert!(entry.first_seen() > 0);
    }

    #[test]
    fn test_scoring() {
        let peer = test_swarm_peer(1);
        let data = SwarmPeerData::new(peer, false);
        let entry = PeerEntry::new(data);

        entry.record_success(Duration::from_millis(50));
        assert!(entry.score() > 0.0);
        assert!(entry.latency().is_some());

        entry.record_timeout();
        // Score went up by 1.0, then down by 1.5
        assert!(entry.score() < 1.0);
    }

    #[test]
    fn test_custom_config() {
        let peer = test_swarm_peer(1);
        let data = SwarmPeerData::new(peer, false);

        // Use lenient config with smaller penalties
        let config = Arc::new(SwarmScoringConfig::lenient());
        let entry = PeerEntry::with_config(data, config);

        entry.record_timeout();
        // Lenient timeout is -0.5 instead of -1.5
        assert!(entry.score() > -1.0);
    }

    #[test]
    fn test_should_ban() {
        let peer = test_swarm_peer(1);
        let data = SwarmPeerData::new(peer, false);

        let mut config = SwarmScoringConfig::default();
        config.ban_threshold = -10.0;
        let entry = PeerEntry::with_config(data, Arc::new(config));

        // Add enough negative score to trigger ban
        for _ in 0..10 {
            entry.record_timeout();
        }

        assert!(entry.should_ban());
    }

    #[test]
    fn test_ban_unban() {
        let peer = test_swarm_peer(1);
        let data = SwarmPeerData::new(peer, false);
        let entry = PeerEntry::new(data);

        entry.ban(Some("test".to_string()));
        assert!(entry.is_banned());
        assert_eq!(entry.ban_info().unwrap().reason(), Some("test"));

        entry.unban();
        assert!(!entry.is_banned());
    }

    #[test]
    fn test_snapshot_roundtrip() {
        let peer = test_swarm_peer(1);
        let data = SwarmPeerData::new(peer, true);
        let entry = PeerEntry::new(data);

        entry.record_success(Duration::from_millis(100));
        entry.add_score(50.0);

        let snapshot = entry.snapshot();
        let restored = PeerEntry::from_snapshot(snapshot);

        assert!((restored.score() - entry.score()).abs() < 0.01);
        assert_eq!(restored.is_full_node(), entry.is_full_node());
    }
}
