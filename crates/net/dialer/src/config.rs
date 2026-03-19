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
    /// When set, the tracker emits `dial_tracker_pending` and `dial_tracker_in_flight`
    /// gauges with a `purpose` label set to this value.
    pub metrics_label: Option<&'static str>,
    /// LRU capacity for short-lived backoff cache (0 = disabled).
    pub backoff_capacity: usize,
    /// Base backoff duration in seconds (first failure).
    pub backoff_base_secs: u64,
    /// Maximum backoff duration in seconds (cap for exponential growth).
    pub backoff_max_secs: u64,
    /// LRU capacity for the ban cache (0 = disabled).
    pub ban_capacity: usize,
    /// Number of consecutive failures before promoting to ban.
    pub ban_after_failures: u32,
    /// Ban TTL in seconds.
    pub ban_ttl_secs: u64,
}

impl Default for DialTrackerConfig {
    fn default() -> Self {
        Self {
            max_pending: 128,
            max_in_flight: 32,
            pending_ttl: Duration::from_secs(60),
            in_flight_timeout: Duration::from_secs(15),
            cleanup_interval: Duration::from_secs(10),
            metrics_label: None,
            backoff_capacity: 0,
            backoff_base_secs: 0,
            backoff_max_secs: 0,
            ban_capacity: 0,
            ban_after_failures: 0,
            ban_ttl_secs: 0,
        }
    }
}
