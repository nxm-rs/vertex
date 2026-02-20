//! Metrics for the hive protocol.

use metrics::{counter, gauge, histogram};
use strum::IntoStaticStr;
use vertex_observability::{
    DURATION_FINE, DURATION_NETWORK, GaugeGuard, HistogramBucketConfig, LabelValue,
    labels::{direction, outcome},
};

use vertex_swarm_net_headers::ProtocolStreamError;

/// Histogram bucket configurations for hive metrics.
///
/// Collect these at recorder install time via
/// [`vertex_observability::install_prometheus_recorder_with_buckets`].
pub const HISTOGRAM_BUCKETS: &[HistogramBucketConfig] = &[
    HistogramBucketConfig {
        suffix: "hive_exchange_duration_seconds",
        buckets: DURATION_NETWORK,
    },
    // Per-phase timing (validation, encoding).
    HistogramBucketConfig {
        suffix: "hive_validation_duration_seconds",
        buckets: DURATION_FINE,
    },
    // Peers per exchange: integer counts (no matching preset).
    HistogramBucketConfig {
        suffix: "hive_peers_per_exchange",
        buckets: &[1.0, 5.0, 10.0, 15.0, 20.0, 25.0, 30.0, 40.0, 50.0, 100.0],
    },
];

/// Peer validation failure reasons.
#[derive(Debug, Clone, Copy, strum::Display, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ValidationFailure {
    OverlayLength,
    SignatureFormat,
    NonceLength,
    PeerValidation,
    /// Peer is our own overlay (self-dial prevention).
    SelfOverlay,
    /// Multiaddrs missing /p2p/ component.
    MissingPeerId,
}

/// Peer validation outcome for metrics labels.
#[derive(Debug, Clone, Copy, strum::Display, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
enum PeerOutcome {
    Valid,
    Invalid,
}

/// Tracks metrics for a single hive exchange.
pub struct HiveMetrics {
    direction: &'static str,
    start: std::time::Instant,
    _active: GaugeGuard,
    peers_valid: u64,
    peers_invalid: u64,
    outcome_recorded: bool,
}

impl HiveMetrics {
    /// Start tracking a new hive exchange.
    pub fn new(dir: &'static str) -> Self {
        counter!("hive_exchanges_total", "direction" => dir).increment(1);

        Self {
            direction: dir,
            start: std::time::Instant::now(),
            _active: GaugeGuard::increment(gauge!("hive_exchanges_active", "direction" => dir)),
            peers_valid: 0,
            peers_invalid: 0,
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

    /// Record a peer validation failure.
    pub fn record_validation_failure(&mut self, reason: ValidationFailure) {
        counter!("hive_peer_validation_failures_total", "reason" => reason.label_value())
            .increment(1);
        self.peers_invalid += 1;
    }

    /// Record successfully validated/sent peers.
    pub fn add_valid_peers(&mut self, count: u64) {
        self.peers_valid += count;
    }

    /// Record a successful exchange.
    pub fn record_success(mut self) {
        // Record peer counts
        if self.direction == direction::INBOUND {
            counter!("hive_peers_received_total", "outcome" => PeerOutcome::Valid.label_value())
                .increment(self.peers_valid);
            counter!("hive_peers_received_total", "outcome" => PeerOutcome::Invalid.label_value())
                .increment(self.peers_invalid);
        } else {
            counter!("hive_peers_sent_total").increment(self.peers_valid);
        }

        // Record exchange outcome
        counter!(
            "hive_exchange_outcomes_total",
            "direction" => self.direction,
            "outcome" => outcome::SUCCESS
        )
        .increment(1);

        histogram!(
            "hive_exchange_duration_seconds",
            "direction" => self.direction,
            "outcome" => outcome::SUCCESS
        )
        .record(self.start.elapsed().as_secs_f64());

        histogram!("hive_peers_per_exchange", "direction" => self.direction)
            .record(self.peers_valid as f64);

        self.outcome_recorded = true;
    }

    /// Record a failed exchange with reason.
    fn record_failure(&mut self, reason: &'static str) {
        counter!(
            "hive_exchange_outcomes_total",
            "direction" => self.direction,
            "outcome" => outcome::FAILURE
        )
        .increment(1);

        counter!(
            "hive_errors_total",
            "direction" => self.direction,
            "reason" => reason
        )
        .increment(1);

        histogram!(
            "hive_exchange_duration_seconds",
            "direction" => self.direction,
            "outcome" => outcome::FAILURE
        )
        .record(self.start.elapsed().as_secs_f64());

        self.outcome_recorded = true;
    }

    /// Record an error that caused the exchange to fail.
    pub fn record_error(mut self, err: &ProtocolStreamError) {
        self.record_failure(err.label_value());
    }
}

impl Drop for HiveMetrics {
    fn drop(&mut self) {
        if !self.outcome_recorded {
            self.record_failure("unknown");
        }
    }
}
