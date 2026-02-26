//! Configuration for DialTracker.

use std::time::Duration;

/// Configuration for a DialTracker instance.
#[derive(Debug, Clone)]
pub struct DialTrackerConfig {
    /// Maximum number of pending dial requests in the queue.
    pub max_pending: usize,
    /// Maximum number of concurrent in-flight dials.
    pub max_in_flight: usize,
    /// TTL for pending entries before they expire.
    pub pending_ttl: Duration,
    /// Timeout for in-flight dials before they are considered timed out.
    pub in_flight_timeout: Duration,
    /// Interval between automatic cleanup of expired entries.
    pub cleanup_interval: Duration,
}

impl Default for DialTrackerConfig {
    fn default() -> Self {
        Self {
            max_pending: 128,
            max_in_flight: 32,
            pending_ttl: Duration::from_secs(60),
            in_flight_timeout: Duration::from_secs(15),
            cleanup_interval: Duration::from_secs(10),
        }
    }
}
