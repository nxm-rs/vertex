//! Arc-per-peer state for lock-free hot paths.

use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::RwLock;
use vertex_net_local::IpCapability;
use vertex_net_peer_store::{BackoffState, DEFAULT_BASE_BACKOFF_SECS, DEFAULT_MAX_BACKOFF_SECS, unix_timestamp_secs};
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_peer_score::{PeerScore, ScoreObserver, SwarmPeerScore, SwarmScoringConfig, SwarmScoringEvent};
use vertex_swarm_primitives::{OverlayAddress, SwarmNodeType};

use crate::ban::BanInfo;
use crate::data::{SwarmPeerData, SwarmPeerRecord};

/// Stale peer threshold in seconds (1 week).
const STALE_THRESHOLD_SECS: u64 = 7 * 24 * 3600;

/// Derive a stable jitter seed from an overlay address.
fn jitter_seed_from_overlay(overlay: &OverlayAddress) -> u64 {
    let bytes = overlay.0.as_slice();
    u64::from_le_bytes(bytes[..8].try_into().unwrap())
}

/// Per-peer state with lock-free scoring and atomic timestamps.
pub(crate) struct PeerEntry {
    peer: RwLock<SwarmPeer>,
    node_type: RwLock<SwarmNodeType>,
    scoring: SwarmPeerScore,
    first_seen: AtomicU64,
    last_seen: AtomicU64,
    /// Unix timestamp of last dial attempt.
    last_dial_attempt: AtomicU64,
    /// Consecutive dial failures (reset on success).
    consecutive_failures: AtomicU32,
    /// Lock-free ban flag for hot-path checks.
    is_banned: AtomicBool,
    /// Ban information (None = not banned).
    ban_info: RwLock<Option<BanInfo>>,
    /// Stable per-peer seed for backoff jitter (derived from overlay address).
    jitter_seed: u64,
}

impl PeerEntry {
    /// Create a new entry with scoring config.
    pub(crate) fn with_config(
        peer: SwarmPeer,
        node_type: SwarmNodeType,
        overlay: OverlayAddress,
        config: Arc<SwarmScoringConfig>,
        observer: Arc<dyn ScoreObserver>,
    ) -> Self {
        let now = unix_timestamp_secs();
        let jitter_seed = jitter_seed_from_overlay(&overlay);
        Self {
            peer: RwLock::new(peer),
            node_type: RwLock::new(node_type),
            scoring: SwarmPeerScore::new(overlay, PeerScore::new(), config, observer),
            first_seen: AtomicU64::new(now),
            last_seen: AtomicU64::new(now),
            last_dial_attempt: AtomicU64::new(0),
            consecutive_failures: AtomicU32::new(0),
            is_banned: AtomicBool::new(false),
            ban_info: RwLock::new(None),
            jitter_seed,
        }
    }

    /// Create from a persistence record with scoring config.
    pub(crate) fn from_record(
        record: SwarmPeerRecord,
        config: Arc<SwarmScoringConfig>,
        observer: Arc<dyn ScoreObserver>,
    ) -> Self {
        let overlay = record.id;
        let jitter_seed = jitter_seed_from_overlay(&overlay);
        Self {
            scoring: SwarmPeerScore::new(
                overlay,
                PeerScore::from(&record.data.scoring),
                config,
                observer,
            ),
            peer: RwLock::new(record.data.peer),
            node_type: RwLock::new(record.data.node_type),
            first_seen: AtomicU64::new(record.first_seen),
            last_seen: AtomicU64::new(record.last_seen),
            last_dial_attempt: AtomicU64::new(record.last_dial_attempt),
            consecutive_failures: AtomicU32::new(record.consecutive_failures),
            is_banned: AtomicBool::new(record.is_banned),
            ban_info: RwLock::new(record.data.ban_info),
            jitter_seed,
        }
    }

    pub(crate) fn swarm_peer(&self) -> SwarmPeer {
        self.peer.read().clone()
    }

    pub(crate) fn ip_capability(&self) -> IpCapability {
        self.peer.read().ip_capability()
    }

    pub(crate) fn node_type(&self) -> SwarmNodeType {
        *self.node_type.read()
    }

    /// Update the peer identity and node type.
    pub(crate) fn update_peer(&self, peer: SwarmPeer, node_type: SwarmNodeType) {
        *self.peer.write() = peer;
        *self.node_type.write() = node_type;
        self.touch();
    }

    pub(crate) fn score(&self) -> f64 {
        self.scoring.score()
    }

    /// Record a scoring event (delegates to SwarmPeerScore).
    pub(crate) fn record_event(&self, event: SwarmScoringEvent) {
        self.scoring.record_event(event);
    }

    /// Record a successful connection with latency.
    pub(crate) fn record_success(&self, latency: Duration) {
        self.scoring.record_success(Some(latency));
        self.reset_failures();
        self.touch();
    }

    /// Set latency sample without affecting score.
    pub(crate) fn set_latency(&self, rtt: Duration) {
        self.scoring.set_latency(rtt);
    }

    pub(crate) fn first_seen(&self) -> u64 {
        self.first_seen.load(Ordering::Relaxed)
    }

    pub(crate) fn last_seen(&self) -> u64 {
        self.last_seen.load(Ordering::Relaxed)
    }

    /// Update last_seen to current time.
    fn touch(&self) {
        self.last_seen.store(unix_timestamp_secs(), Ordering::Relaxed);
    }

    /// Lock-free ban check via AtomicBool.
    pub(crate) fn is_banned(&self) -> bool {
        self.is_banned.load(Ordering::Relaxed)
    }

    /// Ban the peer. Sets both the atomic flag and ban info.
    pub(crate) fn ban(&self, reason: Option<String>) {
        self.is_banned.store(true, Ordering::Relaxed);
        *self.ban_info.write() = Some(BanInfo::new(reason));
    }

    /// Create a persistence record.
    pub(crate) fn to_record(&self, overlay: OverlayAddress) -> SwarmPeerRecord {
        let data = SwarmPeerData {
            peer: self.peer.read().clone(),
            node_type: *self.node_type.read(),
            scoring: self.scoring.snapshot(),
            ban_info: self.ban_info.read().clone(),
        };

        SwarmPeerRecord {
            id: overlay,
            data,
            first_seen: self.first_seen(),
            last_seen: self.last_seen(),
            last_dial_attempt: self.last_dial_attempt(),
            consecutive_failures: self.consecutive_failures(),
            is_banned: self.is_banned(),
        }
    }

    fn record_dial_attempt(&self) {
        self.last_dial_attempt.store(unix_timestamp_secs(), Ordering::Relaxed);
    }

    pub(crate) fn record_dial_failure(&self) {
        self.consecutive_failures.fetch_add(1, Ordering::Relaxed);
        self.record_dial_attempt();
    }

    /// Reset consecutive failures (called on successful connection).
    fn reset_failures(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
    }

    fn last_dial_attempt(&self) -> u64 {
        self.last_dial_attempt.load(Ordering::Relaxed)
    }

    /// Returns None if never attempted, Some(timestamp) otherwise.
    fn last_dial_attempt_time(&self) -> Option<NonZeroU64> {
        NonZeroU64::new(self.last_dial_attempt.load(Ordering::Relaxed))
    }

    pub(crate) fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures.load(Ordering::Relaxed)
    }

    /// Calculate backoff duration based on consecutive failures.
    /// Uses per-peer jitter (±25%) to prevent synchronized retry storms.
    /// Returns None if no backoff needed (no failures or backoff expired).
    pub(crate) fn backoff_remaining(&self) -> Option<Duration> {
        let failures = self.consecutive_failures();
        if failures == 0 {
            return None;
        }

        let last_attempt = self.last_dial_attempt_time()?;
        let state = BackoffState::new(last_attempt.get(), failures);
        state.remaining_jittered(
            unix_timestamp_secs(),
            DEFAULT_BASE_BACKOFF_SECS,
            DEFAULT_MAX_BACKOFF_SECS,
            self.jitter_seed,
        )
    }

    pub(crate) fn is_in_backoff(&self) -> bool {
        self.backoff_remaining().is_some()
    }

    /// Check if peer is stale (no successful connection in threshold period).
    /// Only considers peers stale if they have failures and haven't connected recently.
    pub(crate) fn is_stale(&self) -> bool {
        let failures = self.consecutive_failures();
        if failures == 0 {
            return false;
        }

        let last_seen = self.last_seen();
        let now = unix_timestamp_secs();

        now.saturating_sub(last_seen) > STALE_THRESHOLD_SECS
    }
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
            peer,
            node_type,
            overlay,
            Arc::new(SwarmScoringConfig::default()),
            Arc::new(NoOpScoreObserver),
        )
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
    }

    #[test]
    fn test_custom_config() {
        let peer = test_swarm_peer(1);
        let overlay = OverlayAddress::from(*peer.overlay());
        let config = Arc::new(SwarmScoringConfig::lenient());
        let entry = PeerEntry::with_config(
            peer,
            SwarmNodeType::Client,
            overlay,
            config,
            Arc::new(NoOpScoreObserver),
        );

        assert_eq!(entry.node_type(), SwarmNodeType::Client);
    }

    #[test]
    fn test_ban() {
        let entry = test_entry(1, SwarmNodeType::Client);

        entry.ban(Some("test".to_string()));
        assert!(entry.is_banned());
    }

    #[test]
    fn test_is_banned_atomic() {
        let entry = test_entry(1, SwarmNodeType::Client);
        assert!(!entry.is_banned());

        entry.ban(Some("test".to_string()));
        assert!(entry.is_banned());

        // Verify the atomic flag is set
        assert!(entry.is_banned.load(Ordering::Relaxed));
    }

    #[test]
    fn test_record_roundtrip() {
        let entry = test_entry(1, SwarmNodeType::Storer);
        entry.record_success(Duration::from_millis(100));

        let overlay = OverlayAddress::from(*entry.swarm_peer().overlay());
        let record = entry.to_record(overlay);
        let config = Arc::new(SwarmScoringConfig::default());
        let restored = PeerEntry::from_record(record, config, Arc::new(NoOpScoreObserver));

        assert!((restored.score() - entry.score()).abs() < 0.01);
        assert_eq!(restored.node_type(), entry.node_type());
    }

    #[test]
    fn test_ban_record_roundtrip() {
        let entry = test_entry(1, SwarmNodeType::Client);
        entry.ban(Some("test reason".to_string()));
        assert!(entry.is_banned());

        let overlay = OverlayAddress::from(*entry.swarm_peer().overlay());
        let record = entry.to_record(overlay);
        assert!(record.is_banned);
        assert!(record.data.ban_info.is_some());

        let config = Arc::new(SwarmScoringConfig::default());
        let restored = PeerEntry::from_record(record, config, Arc::new(NoOpScoreObserver));
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
        // base=30s ±25% jitter → max ~37s
        assert!(backoff.as_secs() <= 38);

        entry.record_dial_failure();
        assert_eq!(entry.consecutive_failures(), 2);
        let backoff = entry.backoff_remaining().unwrap();
        // base=60s ±25% jitter → max ~75s
        assert!(backoff.as_secs() <= 76);
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
    fn test_backoff_record_roundtrip() {
        let entry = test_entry(1, SwarmNodeType::Client);

        entry.record_dial_failure();
        entry.record_dial_failure();

        let overlay = OverlayAddress::from(*entry.swarm_peer().overlay());
        let record = entry.to_record(overlay);
        assert_eq!(record.consecutive_failures, 2);
        assert!(record.last_dial_attempt > 0);

        let config = Arc::new(SwarmScoringConfig::default());
        let restored = PeerEntry::from_record(record, config, Arc::new(NoOpScoreObserver));
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
