//! Serializable peer score snapshot for persistence.

use serde::{Deserialize, Serialize};

/// Serializable snapshot of peer score metrics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(bound(
    serialize = "ExtSnap: Serialize",
    deserialize = "ExtSnap: for<'a> Deserialize<'a>"
))]
pub struct PeerScoreSnapshot<ExtSnap = ()> {
    score: f64,
    last_updated: u64,
    connection_successes: u32,
    connection_timeouts: u32,
    connection_refusals: u32,
    handshake_failures: u32,
    protocol_errors: u32,
    latency_sum_nanos: u64,
    latency_samples: u32,
    ext: ExtSnap,
}

impl<ExtSnap: Default> Default for PeerScoreSnapshot<ExtSnap> {
    fn default() -> Self {
        Self {
            score: 0.0,
            last_updated: 0,
            connection_successes: 0,
            connection_timeouts: 0,
            connection_refusals: 0,
            handshake_failures: 0,
            protocol_errors: 0,
            latency_sum_nanos: 0,
            latency_samples: 0,
            ext: ExtSnap::default(),
        }
    }
}

impl<ExtSnap> PeerScoreSnapshot<ExtSnap> {
    /// Create a new snapshot with all fields.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        score: f64,
        last_updated: u64,
        connection_successes: u32,
        connection_timeouts: u32,
        connection_refusals: u32,
        handshake_failures: u32,
        protocol_errors: u32,
        latency_sum_nanos: u64,
        latency_samples: u32,
        ext: ExtSnap,
    ) -> Self {
        Self {
            score,
            last_updated,
            connection_successes,
            connection_timeouts,
            connection_refusals,
            handshake_failures,
            protocol_errors,
            latency_sum_nanos,
            latency_samples,
            ext,
        }
    }

    pub fn score(&self) -> f64 {
        self.score
    }

    pub fn last_updated(&self) -> u64 {
        self.last_updated
    }

    pub fn connection_successes(&self) -> u32 {
        self.connection_successes
    }

    pub fn connection_timeouts(&self) -> u32 {
        self.connection_timeouts
    }

    pub fn connection_refusals(&self) -> u32 {
        self.connection_refusals
    }

    pub fn handshake_failures(&self) -> u32 {
        self.handshake_failures
    }

    pub fn protocol_errors(&self) -> u32 {
        self.protocol_errors
    }

    pub fn latency_sum_nanos(&self) -> u64 {
        self.latency_sum_nanos
    }

    pub fn latency_samples(&self) -> u32 {
        self.latency_samples
    }

    pub fn ext(&self) -> &ExtSnap {
        &self.ext
    }

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
        let snapshot: PeerScoreSnapshot = PeerScoreSnapshot::default();
        assert_eq!(snapshot.score(), 0.0);
        assert_eq!(snapshot.connection_successes(), 0);
        assert_eq!(snapshot.success_rate(), 0.5);
    }

    #[test]
    fn test_success_rate() {
        let snapshot = PeerScoreSnapshot::new(0.0, 0, 8, 2, 0, 0, 0, 0, 0, ());
        assert!((snapshot.success_rate() - 0.8).abs() < 0.01);
    }

    #[test]
    fn test_avg_latency() {
        let snapshot = PeerScoreSnapshot::new(0.0, 0, 0, 0, 0, 0, 0, 300_000_000, 3, ());
        assert_eq!(snapshot.avg_latency_nanos(), Some(100_000_000));

        let no_latency: PeerScoreSnapshot = PeerScoreSnapshot::default();
        assert_eq!(no_latency.avg_latency_nanos(), None);
    }

    #[test]
    fn test_serialization() {
        let snapshot = PeerScoreSnapshot::new(75.5, 12345, 10, 2, 1, 0, 1, 100_000_000, 5, ());
        let json = serde_json::to_string(&snapshot).unwrap();
        let restored: PeerScoreSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, snapshot);
    }
}
