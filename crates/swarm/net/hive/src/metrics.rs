//! Hive-specific metrics (peer counts, validation).
//!
//! Exchange-level metrics (exchanges_total, outcomes, duration) are emitted
//! by the headers crate's `ProtocolMetrics`.

use vertex_metrics::{DURATION_FINE, HistogramBucketConfig};

/// Counter of peer batches discarded by the hive layer.
///
/// Labels:
/// - `reason="bootnode_mode"` - bootnode role discards inbound gossip
///   without validation; counted on the raw wire peer count.
/// - `reason="rate_limited"` - per-peer inbound bucket exhausted; counted
///   on the raw wire peer count.
/// - `reason="verifier_rejected"` - a peer record failed signature or
///   overlay validation.
pub const HIVE_PEERS_DISCARDED_TOTAL: &str = "hive_peers_discarded_total";

/// Label value for [`HIVE_PEERS_DISCARDED_TOTAL`]: bootnode-mode discard.
pub const DISCARD_REASON_BOOTNODE_MODE: &str = "bootnode_mode";

/// Label value for [`HIVE_PEERS_DISCARDED_TOTAL`]: rate-limit discard.
pub const DISCARD_REASON_RATE_LIMITED: &str = "rate_limited";

/// Label value for [`HIVE_PEERS_DISCARDED_TOTAL`]: verifier-rejected discard.
pub const DISCARD_REASON_VERIFIER_REJECTED: &str = "verifier_rejected";

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
