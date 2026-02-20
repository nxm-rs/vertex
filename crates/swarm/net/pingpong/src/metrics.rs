//! Metrics for the pingpong protocol.

use metrics::{counter, histogram};
use vertex_observability::{
    DURATION_NETWORK, HistogramBucketConfig, LabelValue,
    labels::{direction, outcome},
};

use vertex_swarm_net_headers::ProtocolStreamError;

/// Histogram bucket configurations for pingpong metrics.
///
/// Collect these at recorder install time via
/// [`vertex_observability::install_prometheus_recorder_with_buckets`].
pub const HISTOGRAM_BUCKETS: &[HistogramBucketConfig] = &[HistogramBucketConfig {
    suffix: "pingpong_rtt_seconds",
    buckets: DURATION_NETWORK,
}];

/// Tracks metrics for a single pingpong exchange.
pub struct PingpongMetrics {
    direction: &'static str,
    outcome_recorded: bool,
}

impl PingpongMetrics {
    /// Start tracking a new pingpong exchange.
    pub fn new(dir: &'static str) -> Self {
        counter!("pingpong_exchanges_total", "direction" => dir).increment(1);
        Self {
            direction: dir,
            outcome_recorded: false,
        }
    }

    /// Start tracking an inbound exchange.
    pub fn inbound() -> Self {
        Self::new(direction::INBOUND)
    }

    /// Start tracking an outbound exchange.
    pub fn outbound() -> Self {
        Self::new(direction::OUTBOUND)
    }

    /// Record a successful exchange with RTT (outbound only).
    pub fn record_success_with_rtt(mut self, rtt_secs: f64) {
        histogram!("pingpong_rtt_seconds").record(rtt_secs);
        self.record_outcome(outcome::SUCCESS);
        self.outcome_recorded = true;
    }

    /// Record a successful exchange (inbound — no RTT).
    pub fn record_success(mut self) {
        self.record_outcome(outcome::SUCCESS);
        self.outcome_recorded = true;
    }

    /// Record a failed exchange.
    pub fn record_error(mut self, err: &ProtocolStreamError) {
        self.record_outcome(outcome::FAILURE);
        counter!(
            "pingpong_errors_total",
            "direction" => self.direction,
            "reason" => err.label_value()
        )
        .increment(1);
        self.outcome_recorded = true;
    }

    fn record_outcome(&self, outcome: &'static str) {
        counter!(
            "pingpong_exchange_outcomes_total",
            "direction" => self.direction,
            "outcome" => outcome
        )
        .increment(1);
    }
}

impl Drop for PingpongMetrics {
    fn drop(&mut self) {
        if !self.outcome_recorded {
            counter!(
                "pingpong_exchange_outcomes_total",
                "direction" => self.direction,
                "outcome" => outcome::FAILURE
            )
            .increment(1);
            counter!(
                "pingpong_errors_total",
                "direction" => self.direction,
                "reason" => "unknown"
            )
            .increment(1);
        }
    }
}
