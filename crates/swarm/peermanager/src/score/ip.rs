//! IP address-level scoring for abuse prevention.

use alloy_primitives::B256;
use serde::{Deserialize, Serialize};

/// Score and tracking for a single IP address.
///
/// Used to detect abuse patterns that span multiple overlays,
/// such as an attacker changing their nonce but keeping the same IP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpScore {
    /// Current score for this IP.
    pub score: f64,

    /// Unix timestamp of last update.
    pub last_updated_unix: u64,

    /// Overlay addresses that have been seen from this IP.
    pub known_overlays: Vec<B256>,

    /// Total connection attempts from this IP.
    pub connection_attempts: u32,

    /// Number of protocol errors from this IP.
    pub protocol_errors: u32,

    /// Number of overlays from this IP that have been banned.
    pub banned_overlays: u32,

    /// Whether this IP is banned independently of any overlay.
    pub banned: bool,

    /// Reason for IP-level ban if applicable.
    pub ban_reason: Option<String>,
}

impl Default for IpScore {
    fn default() -> Self {
        Self {
            score: 0.0,
            last_updated_unix: current_unix_timestamp(),
            known_overlays: Vec::new(),
            connection_attempts: 0,
            protocol_errors: 0,
            banned_overlays: 0,
            banned: false,
            ban_reason: None,
        }
    }
}

impl IpScore {
    /// Create a new IP score with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an overlay to known overlays, maintaining bounded size.
    pub fn add_known_overlay(&mut self, overlay: B256, max_overlays: usize) {
        if self.known_overlays.contains(&overlay) {
            return;
        }
        if self.known_overlays.len() >= max_overlays {
            self.known_overlays.remove(0);
        }
        self.known_overlays.push(overlay);
    }

    /// Returns true if this IP has suspicious overlay churn.
    ///
    /// High overlay churn from a single IP may indicate an attacker
    /// cycling through identities.
    pub fn has_suspicious_churn(&self, threshold: usize) -> bool {
        self.known_overlays.len() > threshold
    }

    /// Record that an overlay from this IP was banned.
    pub fn record_overlay_ban(&mut self) {
        self.banned_overlays = self.banned_overlays.saturating_add(1);
    }

    /// Record a connection attempt from this IP.
    pub fn record_connection_attempt(&mut self) {
        self.connection_attempts = self.connection_attempts.saturating_add(1);
    }

    /// Record a protocol error from this IP.
    pub fn record_protocol_error(&mut self) {
        self.protocol_errors = self.protocol_errors.saturating_add(1);
    }

    /// Ban this IP with an optional reason.
    pub fn ban(&mut self, reason: Option<String>) {
        self.banned = true;
        self.ban_reason = reason;
    }

    /// Unban this IP.
    pub fn unban(&mut self) {
        self.banned = false;
        self.ban_reason = None;
    }

    /// Returns the ratio of banned overlays to total known overlays.
    ///
    /// A high ratio suggests this IP is associated with bad actors.
    pub fn ban_ratio(&self) -> f64 {
        if self.known_overlays.is_empty() {
            return 0.0;
        }
        self.banned_overlays as f64 / self.known_overlays.len() as f64
    }
}

/// Get current Unix timestamp in seconds.
pub(crate) fn current_unix_timestamp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_overlay(n: u8) -> B256 {
        B256::repeat_byte(n)
    }

    #[test]
    fn test_default_ip_score() {
        let score = IpScore::default();
        assert_eq!(score.score, 0.0);
        assert!(!score.banned);
        assert_eq!(score.known_overlays.len(), 0);
    }

    #[test]
    fn test_add_known_overlay() {
        let mut score = IpScore::default();

        score.add_known_overlay(test_overlay(1), 3);
        score.add_known_overlay(test_overlay(2), 3);
        score.add_known_overlay(test_overlay(3), 3);
        assert_eq!(score.known_overlays.len(), 3);

        // Adding a fourth should evict the first
        score.add_known_overlay(test_overlay(4), 3);
        assert_eq!(score.known_overlays.len(), 3);
        assert!(!score.known_overlays.contains(&test_overlay(1)));
        assert!(score.known_overlays.contains(&test_overlay(4)));
    }

    #[test]
    fn test_add_known_overlay_dedup() {
        let mut score = IpScore::default();
        let overlay = test_overlay(1);

        score.add_known_overlay(overlay, 10);
        score.add_known_overlay(overlay, 10);
        score.add_known_overlay(overlay, 10);

        assert_eq!(score.known_overlays.len(), 1);
    }

    #[test]
    fn test_suspicious_churn() {
        let mut score = IpScore::default();

        for i in 0..5 {
            score.add_known_overlay(test_overlay(i), 100);
        }

        assert!(!score.has_suspicious_churn(5));
        assert!(score.has_suspicious_churn(4));
    }

    #[test]
    fn test_ban_ratio() {
        let mut score = IpScore::default();

        for i in 0..10 {
            score.add_known_overlay(test_overlay(i), 100);
        }
        score.banned_overlays = 3;

        assert_eq!(score.ban_ratio(), 0.3);
    }

    #[test]
    fn test_ban_unban() {
        let mut score = IpScore::default();

        score.ban(Some("Test reason".to_string()));
        assert!(score.banned);
        assert_eq!(score.ban_reason, Some("Test reason".to_string()));

        score.unban();
        assert!(!score.banned);
        assert!(score.ban_reason.is_none());
    }
}
