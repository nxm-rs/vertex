//! IP address-level tracking for abuse prevention.
//!
//! Extracted from ScoreManager to provide standalone IP tracking that can be used
//! alongside NetPeerManager. Tracks behavior patterns across overlay changes to
//! detect malicious actors who change their nonce but keep the same IP.

use std::collections::HashMap;
use std::net::IpAddr;

use alloy_primitives::B256;
use parking_lot::RwLock;
use tracing::{debug, warn};
use vertex_swarm_primitives::OverlayAddress;

use crate::score::IpScore;

/// Configuration for IP-level tracking.
#[derive(Debug, Clone)]
pub struct IpTrackerConfig {
    /// Maximum overlays tracked per IP (default: 50).
    pub max_overlays_per_ip: usize,
    /// Overlay churn threshold for suspicious activity (default: 10).
    pub churn_threshold: usize,
}

impl Default for IpTrackerConfig {
    fn default() -> Self {
        Self {
            max_overlays_per_ip: 50,
            churn_threshold: 10,
        }
    }
}

/// IP address-level tracker for abuse prevention.
///
/// Tracks behavior patterns that span multiple overlays from the same IP,
/// such as an attacker changing their nonce but keeping the same IP.
pub struct IpScoreTracker {
    config: IpTrackerConfig,
    ip_scores: RwLock<HashMap<IpAddr, IpScore>>,
}

impl IpScoreTracker {
    /// Create a new tracker with default config.
    pub fn new() -> Self {
        Self::with_config(IpTrackerConfig::default())
    }

    /// Create with custom configuration.
    pub fn with_config(config: IpTrackerConfig) -> Self {
        Self {
            config,
            ip_scores: RwLock::new(HashMap::new()),
        }
    }

    /// Get a reference to the configuration.
    pub fn config(&self) -> &IpTrackerConfig {
        &self.config
    }

    /// Associate an IP with an overlay.
    ///
    /// Called when we learn the IP address of a connected peer.
    pub fn associate_ip(&self, overlay: OverlayAddress, ip: IpAddr) {
        let mut ip_scores = self.ip_scores.write();
        let ip_score = ip_scores.entry(ip).or_default();
        ip_score.add_known_overlay(B256::from(*overlay), self.config.max_overlays_per_ip);
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
    pub fn record_overlay_banned(&self, overlay: &OverlayAddress) {
        let overlay_b256 = B256::from(**overlay);
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

    /// Record a connection attempt from an IP.
    pub fn record_connection_attempt(&self, ip: IpAddr) {
        let mut scores = self.ip_scores.write();
        let score = scores.entry(ip).or_default();
        score.record_connection_attempt();
    }

    /// Record a protocol error from an IP.
    pub fn record_protocol_error(&self, ip: IpAddr) {
        let mut scores = self.ip_scores.write();
        let score = scores.entry(ip).or_default();
        score.record_protocol_error();
    }

    /// Get IPs that have suspicious overlay churn.
    pub fn suspicious_ips(&self) -> Vec<IpAddr> {
        self.ip_scores
            .read()
            .iter()
            .filter(|(_, score)| score.has_suspicious_churn(self.config.churn_threshold))
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

    /// Get statistics about the IP tracker.
    pub fn stats(&self) -> IpTrackerStats {
        let ip_scores = self.ip_scores.read();
        IpTrackerStats {
            tracked_ips: ip_scores.len(),
            banned_ips: ip_scores.values().filter(|s| s.banned).count(),
            suspicious_ips: ip_scores
                .values()
                .filter(|s| s.has_suspicious_churn(self.config.churn_threshold))
                .count(),
        }
    }

    /// Clear all tracked IPs.
    pub fn clear(&self) {
        self.ip_scores.write().clear();
    }
}

impl Default for IpScoreTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics about IP tracking.
#[derive(Debug, Clone)]
pub struct IpTrackerStats {
    /// Number of IPs being tracked.
    pub tracked_ips: usize,
    /// Number of banned IPs.
    pub banned_ips: usize,
    /// Number of IPs with suspicious overlay churn.
    pub suspicious_ips: usize,
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
    fn test_associate_ip() {
        let tracker = IpScoreTracker::new();
        let overlay = test_overlay(1);
        let ip = test_ip(1);

        tracker.associate_ip(overlay, ip);

        let overlays = tracker.overlays_for_ip(&ip);
        assert!(overlays.contains(&B256::from(*overlay)));
    }

    #[test]
    fn test_ban_ip() {
        let tracker = IpScoreTracker::new();
        let ip = test_ip(1);

        assert!(!tracker.is_ip_banned(&ip));

        tracker.ban_ip(ip, Some("Test".to_string()));
        assert!(tracker.is_ip_banned(&ip));

        tracker.unban_ip(&ip);
        assert!(!tracker.is_ip_banned(&ip));
    }

    #[test]
    fn test_record_overlay_banned() {
        let tracker = IpScoreTracker::new();
        let overlay = test_overlay(1);
        let ip = test_ip(1);

        tracker.associate_ip(overlay, ip);
        tracker.record_overlay_banned(&overlay);

        let ip_score = tracker.get_ip_score(&ip).unwrap();
        assert_eq!(ip_score.banned_overlays, 1);
    }

    #[test]
    fn test_suspicious_ips() {
        let config = IpTrackerConfig {
            max_overlays_per_ip: 100,
            churn_threshold: 5,
        };
        let tracker = IpScoreTracker::with_config(config);
        let ip = test_ip(1);

        // Associate many overlays with one IP
        for i in 0..10 {
            tracker.associate_ip(test_overlay(i), ip);
        }

        let suspicious = tracker.suspicious_ips();
        assert!(suspicious.contains(&ip));
    }

    #[test]
    fn test_stats() {
        let tracker = IpScoreTracker::new();

        for i in 1..=5 {
            tracker.associate_ip(test_overlay(i), test_ip(i));
        }
        tracker.ban_ip(test_ip(1), None);

        let stats = tracker.stats();
        assert_eq!(stats.tracked_ips, 5);
        assert_eq!(stats.banned_ips, 1);
    }
}
