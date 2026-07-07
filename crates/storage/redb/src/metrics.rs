//! Metrics constants and histogram buckets for database operations.

use vertex_metrics::{HistogramBucketConfig, POLL_DURATION};

/// Histogram bucket configurations for database operation metrics.
///
/// Database operations share the poll-loop range (10us-1s), so they reuse the
/// [`POLL_DURATION`] preset rather than a bespoke array.
pub const HISTOGRAM_BUCKETS: &[HistogramBucketConfig] = &[
    HistogramBucketConfig {
        suffix: "db_operation_duration_seconds",
        buckets: POLL_DURATION,
    },
    HistogramBucketConfig {
        suffix: "db_tx_duration_seconds",
        buckets: POLL_DURATION,
    },
    HistogramBucketConfig {
        suffix: "db_tx_commit_duration_seconds",
        buckets: POLL_DURATION,
    },
];

/// Database operation names for label values.
pub mod operation {
    pub const GET: &str = "get";
    pub const PUT: &str = "put";
    pub const DELETE: &str = "delete";
    pub const CLEAR: &str = "clear";
    pub const ENTRIES: &str = "entries";
    pub const KEYS: &str = "keys";
    pub const COUNT: &str = "count";
    pub const COMMIT: &str = "commit";
}

/// Transaction mode label values.
pub mod mode {
    pub const READ: &str = "read";
    pub const WRITE: &str = "write";
}
