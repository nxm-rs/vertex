//! Metrics for the hive protocol.

use metrics::{counter, gauge, histogram};
use strum::IntoStaticStr;
use vertex_swarm_observability::{
    GaugeGuard,
    common::{direction, outcome},
};

use crate::error::HiveError;

/// Peer validation failure reasons.
#[derive(Debug, Clone, Copy, IntoStaticStr)]
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

/// Peer validation outcome labels.
mod peer_outcome {
    pub(super) const VALID: &str = "valid";
    pub(super) const INVALID: &str = "invalid";
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
        let label: &'static str = (&reason).into();
        counter!("hive_peer_validation_failures_total", "reason" => label).increment(1);
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
            counter!("hive_peers_received_total", "outcome" => peer_outcome::VALID)
                .increment(self.peers_valid);
            counter!("hive_peers_received_total", "outcome" => peer_outcome::INVALID)
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
    pub fn record_error(mut self, err: &HiveError) {
        self.record_failure(err.label());
    }
}

impl Drop for HiveMetrics {
    fn drop(&mut self) {
        if !self.outcome_recorded {
            self.record_failure("unknown");
        }
    }
}
