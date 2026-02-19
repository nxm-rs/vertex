//! Serializable peer score snapshot for persistence.

use serde::{Deserialize, Serialize};

/// Serializable snapshot of peer score metrics.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PeerScoreSnapshot {
    pub score: f64,
    pub last_updated: u64,
    pub connection_successes: u32,
    pub connection_timeouts: u32,
    pub connection_refusals: u32,
    pub handshake_failures: u32,
    pub protocol_errors: u32,
    pub latency_sum_nanos: u64,
    pub latency_samples: u32,
}

impl PeerScoreSnapshot {
    pub fn total_connection_attempts(&self) -> u32 {
        self.connection_successes
            + self.connection_timeouts
            + self.connection_refusals
            + self.handshake_failures
    }

    /// Returns 0.5 (neutral) if no attempts recorded.
    pub fn success_rate(&self) -> f64 {
        let total = self.total_connection_attempts();
        if total == 0 {
            return 0.5;
        }
        self.connection_successes as f64 / total as f64
    }

    pub fn avg_latency_nanos(&self) -> Option<u64> {
        if self.latency_samples == 0 {
            return None;
        }
        Some(self.latency_sum_nanos / self.latency_samples as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default() {
        let snapshot = PeerScoreSnapshot::default();
        assert_eq!(snapshot.score, 0.0);
        assert_eq!(snapshot.connection_successes, 0);
        assert_eq!(snapshot.success_rate(), 0.5);
    }

    #[test]
    fn test_success_rate() {
        let snapshot = PeerScoreSnapshot {
            connection_successes: 8,
            connection_timeouts: 2,
            ..Default::default()
        };
        assert!((snapshot.success_rate() - 0.8).abs() < 0.01);
    }

    #[test]
    fn test_avg_latency() {
        let snapshot = PeerScoreSnapshot {
            latency_sum_nanos: 300_000_000,
            latency_samples: 3,
            ..Default::default()
        };
        assert_eq!(snapshot.avg_latency_nanos(), Some(100_000_000));

        let no_latency = PeerScoreSnapshot::default();
        assert_eq!(no_latency.avg_latency_nanos(), None);
    }

    #[test]
    fn test_serialization() {
        let snapshot = PeerScoreSnapshot {
            score: 75.5,
            last_updated: 12345,
            connection_successes: 10,
            connection_timeouts: 2,
            connection_refusals: 1,
            handshake_failures: 0,
            protocol_errors: 1,
            latency_sum_nanos: 100_000_000,
            latency_samples: 5,
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        let restored: PeerScoreSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, snapshot);
    }
}
