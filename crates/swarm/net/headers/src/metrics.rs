//! Unified protocol exchange metrics for all headered protocols.

use std::time::Instant;

use metrics::{counter, histogram};
use vertex_metrics::labels::{direction, outcome};
use vertex_observability::HistogramBucketConfig;

/// Histogram bucket configurations for protocol exchange metrics.
pub const HISTOGRAM_BUCKETS: &[HistogramBucketConfig] = &[HistogramBucketConfig {
        suffix: "protocol_exchange_duration_seconds",
        buckets: &[
            0.010, 0.025, 0.050, 0.100, 0.250, 0.500, 1.0, 2.5, 5.0, 10.0, 30.0,
        ],
    }];

/// Tracks metrics for a single protocol exchange (inbound or outbound).
///
/// Created automatically by `Inbound<P>` / `Outbound<P>` wrappers.
/// Records exchange count on creation, duration and outcome on completion.
/// Drop guard records outcome=unknown if not explicitly recorded.
pub(crate) struct ProtocolMetrics {
    protocol: &'static str,
    direction: &'static str,
    start: Instant,
    outcome_recorded: bool,
}

impl ProtocolMetrics {
    pub(crate) fn new(protocol: &'static str, dir: &'static str) -> Self {
        counter!("protocol_exchanges_total", "protocol" => protocol, "direction" => dir)
            .increment(1);

        Self {
            protocol,
            direction: dir,
            start: Instant::now(),
            outcome_recorded: false,
        }
    }

    #[inline]
    pub(crate) fn inbound(protocol: &'static str) -> Self {
        Self::new(protocol, direction::INBOUND)
    }

    #[inline]
    pub(crate) fn outbound(protocol: &'static str) -> Self {
        Self::new(protocol, direction::OUTBOUND)
    }

    pub(crate) fn record_success(&mut self) {
        counter!(
            "protocol_exchange_outcomes_total",
            "protocol" => self.protocol,
            "direction" => self.direction,
            "outcome" => outcome::SUCCESS,
        )
        .increment(1);

        histogram!(
            "protocol_exchange_duration_seconds",
            "protocol" => self.protocol,
            "direction" => self.direction,
        )
        .record(self.start.elapsed().as_secs_f64());

        self.outcome_recorded = true;
    }

    pub(crate) fn record_error(&mut self) {
        counter!(
            "protocol_exchange_outcomes_total",
            "protocol" => self.protocol,
            "direction" => self.direction,
            "outcome" => outcome::FAILURE,
        )
        .increment(1);

        histogram!(
            "protocol_exchange_duration_seconds",
            "protocol" => self.protocol,
            "direction" => self.direction,
        )
        .record(self.start.elapsed().as_secs_f64());

        self.outcome_recorded = true;
    }
}

impl Drop for ProtocolMetrics {
    fn drop(&mut self) {
        if !self.outcome_recorded {
            counter!(
                "protocol_exchange_outcomes_total",
                "protocol" => self.protocol,
                "direction" => self.direction,
                "outcome" => "unknown",
            )
            .increment(1);
        }
    }
}
