//! Hive-specific metrics (peer counts, validation, broadcasts).
//!
//! Exchange-level metrics (exchanges_total, outcomes, duration) are handled
//! automatically by the headers crate's `ProtocolMetrics`.

use metrics::counter;
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

/// Discard reasons for inbound hive peer entries.
///
/// Used as the `reason` label on `hive_peers_discarded_total`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum DiscardReason {
    /// Bootnode mode: ingestion is intentionally suppressed.
    BootnodeMode,
    /// Local IP capability cannot reach the gossiped peer.
    Unreachable,
    /// Peer failed validation (signature, overlay, etc.).
    Invalid,
    /// Peer is currently banned by local policy.
    Banned,
}

/// Record that a hive peer broadcast was sent to a remote.
pub fn record_broadcast_sent() {
    counter!("hive_broadcasts_sent_total").increment(1);
}

/// Record that an inbound hive peer entry was discarded with the given reason.
pub fn record_peer_discarded(reason: DiscardReason) {
    let reason_label: &'static str = reason.into();
    counter!("hive_peers_discarded_total", "reason" => reason_label).increment(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers_do_not_panic() {
        record_broadcast_sent();
        record_peer_discarded(DiscardReason::BootnodeMode);
        record_peer_discarded(DiscardReason::Unreachable);
        record_peer_discarded(DiscardReason::Invalid);
        record_peer_discarded(DiscardReason::Banned);
    }

    #[test]
    fn discard_reason_labels_are_snake_case() {
        let label: &'static str = DiscardReason::BootnodeMode.into();
        assert_eq!(label, "bootnode_mode");
        let label: &'static str = DiscardReason::Unreachable.into();
        assert_eq!(label, "unreachable");
    }
}
