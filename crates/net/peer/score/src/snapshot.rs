//! Serializable peer score snapshot for persistence.

use serde::{Deserialize, Serialize};

/// Serializable snapshot of peer score state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PeerScoreSnapshot {
    pub score: f64,
    pub latency_sum_nanos: u64,
    pub latency_samples: u32,
}

impl PeerScoreSnapshot {
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
        assert_eq!(snapshot.avg_latency_nanos(), None);
    }

    #[test]
    fn test_avg_latency() {
        let snapshot = PeerScoreSnapshot {
            score: 0.0,
            latency_sum_nanos: 300_000_000,
            latency_samples: 3,
        };
        assert_eq!(snapshot.avg_latency_nanos(), Some(100_000_000));
    }

    #[test]
    fn test_serialization() {
        let snapshot = PeerScoreSnapshot {
            score: 75.5,
            latency_sum_nanos: 100_000_000,
            latency_samples: 5,
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        let restored: PeerScoreSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, snapshot);
    }
}
