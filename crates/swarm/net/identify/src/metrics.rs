//! Metrics for the identify protocol.

use metrics::{counter, histogram};
use vertex_observability::{
    DURATION_SECONDS, HistogramBucketConfig, LabelValue,
    labels::{direction, outcome},
};

/// Histogram bucket configurations for identify metrics.
pub const HISTOGRAM_BUCKETS: &[HistogramBucketConfig] = &[HistogramBucketConfig {
    suffix: "identify_duration_seconds",
    buckets: DURATION_SECONDS,
}];

/// Identify error classification for metrics labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum IdentifyErrorKind {
    Timeout,
    Apply,
}

/// Record a received identify event with the remote peer's agent version.
///
/// Increments `identify_received_total` with `purpose` and `agent_version`
/// labels so that the distribution of agent versions can be queried per-swarm
/// via `sum by (agent_version) (identify_received_total{purpose="topology"})`.
pub(crate) fn record_received(
    purpose: &'static str,
    agent_version: &str,
    duration: std::time::Duration,
) {
    counter!(
        "identify_received_total",
        "purpose" => purpose,
        "agent_version" => normalize_agent_version(agent_version),
    )
    .increment(1);

    histogram!(
        "identify_duration_seconds",
        "purpose" => purpose,
        "direction" => direction::INBOUND,
        "outcome" => outcome::SUCCESS,
    )
    .record(duration.as_secs_f64());
}

/// Record an outbound identify push event.
pub(crate) fn record_pushed(purpose: &'static str) {
    counter!("identify_pushed_total", "purpose" => purpose).increment(1);
}

/// Record an outbound identify sent event.
pub(crate) fn record_sent(purpose: &'static str) {
    counter!("identify_sent_total", "purpose" => purpose).increment(1);
}

/// Record an identify error.
pub(crate) fn record_error(
    purpose: &'static str,
    kind: IdentifyErrorKind,
    duration: std::time::Duration,
) {
    counter!(
        "identify_error_total",
        "purpose" => purpose,
        "kind" => kind.label_value(),
    )
    .increment(1);

    histogram!(
        "identify_duration_seconds",
        "purpose" => purpose,
        "direction" => direction::INBOUND,
        "outcome" => outcome::FAILURE,
    )
    .record(duration.as_secs_f64());
}

/// Normalize agent version strings to a bounded set of label values.
///
/// High-cardinality labels can cause memory issues in Prometheus. This function
/// caps the label length and replaces empty values with "unknown".
fn normalize_agent_version(agent_version: &str) -> String {
    let trimmed = agent_version.trim();
    if trimmed.is_empty() {
        return "unknown".to_string();
    }
    // Cap at 64 characters to bound cardinality from adversarial peers.
    if trimmed.len() > 64 {
        trimmed[..64].to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_empty_agent_version() {
        assert_eq!(normalize_agent_version(""), "unknown");
        assert_eq!(normalize_agent_version("  "), "unknown");
    }

    #[test]
    fn normalize_normal_agent_version() {
        assert_eq!(
            normalize_agent_version("vertex/0.1.0"),
            "vertex/0.1.0"
        );
        assert_eq!(
            normalize_agent_version("bee/2.3.0-abc123"),
            "bee/2.3.0-abc123"
        );
    }

    #[test]
    fn normalize_long_agent_version() {
        let long = "a".repeat(100);
        let normalized = normalize_agent_version(&long);
        assert_eq!(normalized.len(), 64);
    }
}
