//! Pingpong-specific metrics (RTT).
//!
//! Exchange-level metrics (exchanges_total, outcomes, duration) are handled
//! automatically by the headers crate's `ProtocolMetrics`.

use vertex_observability::{DURATION_NETWORK, HistogramBucketConfig};

/// Histogram bucket configurations for pingpong-specific metrics.
pub const HISTOGRAM_BUCKETS: &[HistogramBucketConfig] = &[HistogramBucketConfig {
    suffix: "pingpong_rtt_seconds",
    buckets: DURATION_NETWORK,
}];
