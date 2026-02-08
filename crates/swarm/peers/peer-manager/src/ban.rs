//! Ban information for peers.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Ban metadata for a peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BanInfo {
    banned_at_unix: u64,
    reason: Option<String>,
}

impl BanInfo {
    /// Create new ban info with current timestamp.
    pub fn new(reason: Option<String>) -> Self {
        Self {
            banned_at_unix: unix_timestamp_secs(),
            reason,
        }
    }

    /// Unix timestamp when the peer was banned.
    pub fn banned_at_unix(&self) -> u64 {
        self.banned_at_unix
    }

    /// Optional reason for the ban.
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ban_info_new() {
        let info = BanInfo::new(Some("misbehaving".to_string()));
        assert!(info.banned_at_unix() > 0);
        assert_eq!(info.reason(), Some("misbehaving"));
    }

    #[test]
    fn test_ban_info_no_reason() {
        let info = BanInfo::new(None);
        assert!(info.banned_at_unix() > 0);
        assert_eq!(info.reason(), None);
    }

    #[test]
    fn test_serialization() {
        let info = BanInfo::new(Some("test".to_string()));
        let json = serde_json::to_string(&info).unwrap();
        let restored: BanInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, info);
    }
}
