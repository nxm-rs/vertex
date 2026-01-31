//! Central score manager with double-checked locking.
//!
//! The `ScoreManager` provides:
//! - Per-peer state registry with minimal contention (double-checked locking)
//! - IP-level tracking for abuse prevention (separate cold storage)
//! - Handle factory for vertex-swarm-client integration

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

use alloy_primitives::B256;
use parking_lot::RwLock;
use tracing::{debug, warn};
use vertex_primitives::OverlayAddress;

use super::config::{ScoreConfig, ScoreWeights};
use super::handle::ScoreHandle;
use super::ip::IpScore;
use super::peer::{PeerScoreSnapshot, PeerScoreState};

/// Central score manager for peers and IPs.
///
/// The manager uses double-checked locking for the peer registry:
/// - Fast path: Read lock to get existing `Arc<PeerScoreState>`
/// - Slow path: Write lock only on first access per peer
///
/// After the first access, all operations go through the `Arc`-wrapped
/// state with no lock contention.
pub struct ScoreManager {
    config: ScoreConfig,
    weights: Arc<ScoreWeights>,
    peers: RwLock<HashMap<OverlayAddress, Arc<PeerScoreState>>>,
    ip_scores: RwLock<HashMap<IpAddr, IpScore>>,
}

impl ScoreManager {
    /// Create a new score manager with default config.
    pub fn new() -> Self {
        Self::with_config(ScoreConfig::default())
    }

    /// Create with custom configuration.
    pub fn with_config(config: ScoreConfig) -> Self {
        let weights = Arc::new(config.weights.clone());
        Self {
            config,
            weights,
            peers: RwLock::new(HashMap::new()),
            ip_scores: RwLock::new(HashMap::new()),
        }
    }

    /// Get a reference to the configuration.
    pub fn config(&self) -> &ScoreConfig {
        &self.config
    }

    /// Get or create the score state for a peer.
    ///
    /// Uses double-checked locking: read lock first (fast path),
    /// write lock only if the peer doesn't exist yet (slow path).
    pub fn get_or_create_peer(&self, peer: OverlayAddress) -> Arc<PeerScoreState> {
        // Fast path: read lock
        {
            let peers = self.peers.read();
            if let Some(state) = peers.get(&peer) {
                return Arc::clone(state);
            }
        }

        // Slow path: write lock (only on first access)
        let mut peers = self.peers.write();
        peers
            .entry(peer)
            .or_insert_with(|| {
                debug!(?peer, "creating new peer score state");
                Arc::new(PeerScoreState::new(peer))
            })
            .clone()
    }

    /// Get a handle for recording events on a peer.
    ///
    /// The handle is cheap to clone and can be stored in per-peer
    /// connection state within vertex-swarm-client.
    pub fn handle_for(&self, peer: OverlayAddress) -> ScoreHandle {
        let state = self.get_or_create_peer(peer);
        ScoreHandle::new(state, Arc::clone(&self.weights))
    }

    /// Get the score state for a peer if it exists.
    pub fn get_peer(&self, peer: &OverlayAddress) -> Option<Arc<PeerScoreState>> {
        self.peers.read().get(peer).cloned()
    }

    /// Get the current score for a peer.
    pub fn get_score(&self, peer: &OverlayAddress) -> f64 {
        self.peers
            .read()
            .get(peer)
            .map(|s| s.score())
            .unwrap_or(0.0)
    }

    /// Check if a peer should be banned based on score.
    pub fn should_ban(&self, peer: &OverlayAddress) -> bool {
        self.get_score(peer) < self.config.ban_threshold
    }

    /// Check if a peer should be deprioritized.
    pub fn should_deprioritize(&self, peer: &OverlayAddress) -> bool {
        self.get_score(peer) < self.config.deprioritize_threshold
    }

    /// Rank overlays by score (highest first).
    ///
    /// Unknown peers are assigned a neutral score of 0.0.
    pub fn rank_overlays(&self, overlays: &[OverlayAddress]) -> Vec<(OverlayAddress, f64)> {
        let peers = self.peers.read();
        let mut ranked: Vec<_> = overlays
            .iter()
            .map(|o| {
                let score = peers.get(o).map(|s| s.score()).unwrap_or(0.0);
                (*o, score)
            })
            .collect();

        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranked
    }

    /// Associate an IP with an overlay.
    ///
    /// This is called when we learn the IP address of a connected peer.
    /// The association is stored for abuse detection.
    pub fn associate_ip(&self, peer: OverlayAddress, ip: IpAddr) {
        let mut ip_scores = self.ip_scores.write();
        let ip_score = ip_scores.entry(ip).or_default();
        ip_score.add_known_overlay(B256::from(*peer), self.config.max_overlays_per_ip);
    }

    /// Check if an IP is banned.
    pub fn is_ip_banned(&self, ip: &IpAddr) -> bool {
        self.ip_scores
            .read()
            .get(ip)
            .map(|s| s.banned)
            .unwrap_or(false)
    }

    /// Ban an IP address directly.
    pub fn ban_ip(&self, ip: IpAddr, reason: Option<String>) {
        warn!(%ip, ?reason, "banning IP address");
        let mut scores = self.ip_scores.write();
        let score = scores.entry(ip).or_default();
        score.ban(reason);
    }

    /// Unban an IP address.
    pub fn unban_ip(&self, ip: &IpAddr) {
        if let Some(score) = self.ip_scores.write().get_mut(ip) {
            score.unban();
        }
    }

    /// Record that an overlay was banned (updates IP tracking).
    pub fn record_overlay_banned(&self, peer: &OverlayAddress) {
        let overlay_b256 = B256::from(**peer);
        let mut ip_scores = self.ip_scores.write();

        for score in ip_scores.values_mut() {
            if score.known_overlays.contains(&overlay_b256) {
                score.record_overlay_ban();
                debug!(
                    banned_overlays = score.banned_overlays,
                    "recorded overlay ban for IP"
                );
            }
        }
    }

    /// Get IPs that have suspicious overlay churn.
    pub fn suspicious_ips(&self, churn_threshold: usize) -> Vec<IpAddr> {
        self.ip_scores
            .read()
            .iter()
            .filter(|(_, score)| score.has_suspicious_churn(churn_threshold))
            .map(|(ip, _)| *ip)
            .collect()
    }

    /// Get all overlays associated with an IP.
    pub fn overlays_for_ip(&self, ip: &IpAddr) -> Vec<B256> {
        self.ip_scores
            .read()
            .get(ip)
            .map(|s| s.known_overlays.clone())
            .unwrap_or_default()
    }

    /// Get the IP score for an address.
    pub fn get_ip_score(&self, ip: &IpAddr) -> Option<IpScore> {
        self.ip_scores.read().get(ip).cloned()
    }

    /// Create snapshots of all peer scores for persistence.
    pub fn snapshots(&self) -> Vec<(OverlayAddress, PeerScoreSnapshot)> {
        self.peers
            .read()
            .iter()
            .map(|(peer, state)| (*peer, state.snapshot()))
            .collect()
    }

    /// Restore peer scores from snapshots.
    pub fn restore_snapshots(
        &self,
        snapshots: impl IntoIterator<Item = (OverlayAddress, PeerScoreSnapshot)>,
    ) {
        let mut peers = self.peers.write();
        for (peer, snapshot) in snapshots {
            let state = peers
                .entry(peer)
                .or_insert_with(|| Arc::new(PeerScoreState::new(peer)));
            state.restore(&snapshot);
        }
    }

    /// Get statistics about the score manager.
    pub fn stats(&self) -> ScoreManagerStats {
        let peers = self.peers.read();
        let ip_scores = self.ip_scores.read();

        let scores: Vec<f64> = peers.values().map(|s| s.score()).collect();
        let avg_score = if scores.is_empty() {
            0.0
        } else {
            scores.iter().sum::<f64>() / scores.len() as f64
        };

        ScoreManagerStats {
            tracked_peers: peers.len(),
            tracked_ips: ip_scores.len(),
            avg_peer_score: avg_score,
            peers_below_threshold: scores
                .iter()
                .filter(|s| **s < self.config.deprioritize_threshold)
                .count(),
            banned_ips: ip_scores.values().filter(|s| s.banned).count(),
        }
    }

    /// Remove a peer from the registry.
    ///
    /// Note: This removes the state entirely. Use with caution.
    pub fn remove_peer(&self, peer: &OverlayAddress) {
        self.peers.write().remove(peer);
    }

    /// Clear all peer and IP scores.
    pub fn clear(&self) {
        self.peers.write().clear();
        self.ip_scores.write().clear();
    }
}

impl Default for ScoreManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics about the score manager state.
#[derive(Debug, Clone)]
pub struct ScoreManagerStats {
    /// Number of peers with tracked scores.
    pub tracked_peers: usize,
    /// Number of IPs with tracked scores.
    pub tracked_ips: usize,
    /// Average peer score.
    pub avg_peer_score: f64,
    /// Number of peers below deprioritize threshold.
    pub peers_below_threshold: usize,
    /// Number of banned IPs.
    pub banned_ips: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_overlay(n: u8) -> OverlayAddress {
        OverlayAddress::from(B256::repeat_byte(n))
    }

    fn test_ip(n: u8) -> IpAddr {
        format!("192.168.1.{}", n).parse().unwrap()
    }

    #[test]
    fn test_get_or_create_peer() {
        let manager = ScoreManager::new();
        let peer = test_overlay(1);

        // First access creates
        let state1 = manager.get_or_create_peer(peer);
        assert_eq!(state1.peer(), peer);

        // Second access returns same Arc
        let state2 = manager.get_or_create_peer(peer);
        assert!(Arc::ptr_eq(&state1, &state2));
    }

    #[test]
    fn test_handle_for() {
        let manager = ScoreManager::new();
        let peer = test_overlay(1);

        let handle = manager.handle_for(peer);
        assert_eq!(handle.peer(), peer);

        // Handle shares state with manager
        handle.record_connection_success(50);
        assert!(manager.get_score(&peer) > 0.0);
    }

    #[test]
    fn test_rank_overlays() {
        let manager = ScoreManager::new();
        let peers: Vec<_> = (1..=3).map(test_overlay).collect();

        // Give different scores
        manager.handle_for(peers[0]).record_connection_success(50);
        for _ in 0..3 {
            manager.handle_for(peers[1]).record_connection_success(50);
        }
        manager.handle_for(peers[2]).record_protocol_error();

        let ranked = manager.rank_overlays(&peers);

        // peers[1] should be first (highest score)
        assert_eq!(ranked[0].0, peers[1]);
        // peers[0] should be second
        assert_eq!(ranked[1].0, peers[0]);
        // peers[2] should be last (negative score)
        assert_eq!(ranked[2].0, peers[2]);
    }

    #[test]
    fn test_ip_association() {
        let manager = ScoreManager::new();
        let peer = test_overlay(1);
        let ip = test_ip(1);

        manager.associate_ip(peer, ip);

        let overlays = manager.overlays_for_ip(&ip);
        assert!(overlays.contains(&B256::from(*peer)));
    }

    #[test]
    fn test_ban_ip() {
        let manager = ScoreManager::new();
        let ip = test_ip(1);

        assert!(!manager.is_ip_banned(&ip));

        manager.ban_ip(ip, Some("Test".to_string()));
        assert!(manager.is_ip_banned(&ip));

        manager.unban_ip(&ip);
        assert!(!manager.is_ip_banned(&ip));
    }

    #[test]
    fn test_record_overlay_banned() {
        let manager = ScoreManager::new();
        let peer = test_overlay(1);
        let ip = test_ip(1);

        manager.associate_ip(peer, ip);
        manager.record_overlay_banned(&peer);

        let ip_score = manager.get_ip_score(&ip).unwrap();
        assert_eq!(ip_score.banned_overlays, 1);
    }

    #[test]
    fn test_suspicious_ips() {
        let manager = ScoreManager::new();
        let ip = test_ip(1);

        // Associate many overlays with one IP
        for i in 0..10 {
            manager.associate_ip(test_overlay(i), ip);
        }

        let suspicious = manager.suspicious_ips(5);
        assert!(suspicious.contains(&ip));

        let not_suspicious = manager.suspicious_ips(15);
        assert!(!not_suspicious.contains(&ip));
    }

    #[test]
    fn test_snapshot_restore() {
        let manager = ScoreManager::new();
        let peer = test_overlay(1);

        let handle = manager.handle_for(peer);
        handle.record_connection_success(50);
        handle.record_connection_success(60);
        handle.record_protocol_error();

        let snapshots = manager.snapshots();
        assert_eq!(snapshots.len(), 1);

        // Create new manager and restore
        let manager2 = ScoreManager::new();
        manager2.restore_snapshots(snapshots);

        let state = manager2.get_peer(&peer).unwrap();
        assert_eq!(state.connection_successes(), 2);
        assert_eq!(state.protocol_errors(), 1);
    }

    #[test]
    fn test_stats() {
        let manager = ScoreManager::new();

        for i in 1..=5 {
            let handle = manager.handle_for(test_overlay(i));
            handle.record_connection_success(50);
            manager.associate_ip(test_overlay(i), test_ip(i));
        }

        let stats = manager.stats();
        assert_eq!(stats.tracked_peers, 5);
        assert_eq!(stats.tracked_ips, 5);
        assert!(stats.avg_peer_score > 0.0);
    }

    #[test]
    fn test_concurrent_access() {
        use std::thread;

        let manager = Arc::new(ScoreManager::new());
        let mut handles = vec![];

        // Multiple threads getting handles for different peers
        for i in 0..10u8 {
            let manager = Arc::clone(&manager);
            handles.push(thread::spawn(move || {
                let peer = test_overlay(i);
                let handle = manager.handle_for(peer);
                for _ in 0..100 {
                    handle.record_connection_success(50);
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(manager.stats().tracked_peers, 10);
    }
}
