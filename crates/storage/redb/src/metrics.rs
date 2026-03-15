//! Metrics constants and histogram buckets for database operations.

use vertex_observability::HistogramBucketConfig;

/// Histogram bucket configurations for database operation metrics.
pub const HISTOGRAM_BUCKETS: &[HistogramBucketConfig] = &[
    HistogramBucketConfig {
        suffix: "db_operation_duration_seconds",
        buckets: DURATION_DB,
    },
    HistogramBucketConfig {
        suffix: "db_tx_duration_seconds",
        buckets: DURATION_DB,
    },
    HistogramBucketConfig {
        suffix: "db_tx_commit_duration_seconds",
        buckets: DURATION_DB,
    },
];

/// Database operation duration: 10us-1s (10 buckets).
const DURATION_DB: &[f64] = &[
    0.00001, 0.0001, 0.0005, 0.001, 0.005, 0.010, 0.050, 0.100, 0.500, 1.0,
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
