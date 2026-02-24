//! Hive-specific metrics (peer counts, validation).
//!
//! Exchange-level metrics (exchanges_total, outcomes, duration) are handled
//! automatically by the headers crate's `ProtocolMetrics`.

use vertex_observability::{DURATION_FINE, HistogramBucketConfig};

/// Histogram bucket configurations for hive-specific metrics.
pub const HISTOGRAM_BUCKETS: &[HistogramBucketConfig] = &[
    HistogramBucketConfig {
        suffix: "hive_validation_duration_seconds",
        buckets: DURATION_FINE,
    },
    HistogramBucketConfig {
        suffix: "hive_peers_per_exchange",
        buckets: &[1.0, 5.0, 10.0, 15.0, 20.0, 25.0, 30.0, 40.0, 50.0, 100.0],
    },
];
