//! Metrics for the handshake protocol.

use metrics::{counter, gauge, histogram};
use vertex_observability::{
    GaugeGuard, LabelValue,
    labels::{direction, outcome},
};
use vertex_swarm_peer::SwarmNodeType;

use crate::HandshakeError;

/// Tracks metrics for a single handshake operation.
pub struct HandshakeMetrics {
    direction: &'static str,
    start: std::time::Instant,
    _active: GaugeGuard,
    outcome_recorded: bool,
}

impl HandshakeMetrics {
    /// Start tracking a new handshake.
    pub fn new(dir: &'static str) -> Self {
        counter!("handshake_attempts_total", "direction" => dir).increment(1);

        Self {
            direction: dir,
            start: std::time::Instant::now(),
            _active: GaugeGuard::increment(gauge!("handshake_active", "direction" => dir)),
            outcome_recorded: false,
        }
    }

    /// Start tracking an inbound handshake.
    pub fn inbound() -> Self {
        Self::new(direction::INBOUND)
    }

    /// Start tracking an outbound handshake.
    pub fn outbound() -> Self {
        Self::new(direction::OUTBOUND)
    }

    /// Record a successful handshake.
    pub fn record_success(mut self, peer_node_type: SwarmNodeType) {
        // Map bootnode to client for metrics (bootnodes behave like clients)
        let node_type_label: &'static str = match peer_node_type {
            SwarmNodeType::Storer => SwarmNodeType::Storer.into(),
            SwarmNodeType::Client | SwarmNodeType::Bootnode => SwarmNodeType::Client.into(),
        };

        counter!(
            "handshake_success_total",
            "direction" => self.direction,
            "node_type" => node_type_label
        )
        .increment(1);

        histogram!(
            "handshake_duration_seconds",
            "direction" => self.direction,
            "outcome" => outcome::SUCCESS
        )
        .record(self.start.elapsed().as_secs_f64());

        self.outcome_recorded = true;
    }

    /// Record a failed handshake.
    pub fn record_failure(mut self, error: &HandshakeError) {
        counter!(
            "handshake_failure_total",
            "direction" => self.direction,
            "reason" => error.label_value()
        )
        .increment(1);

        histogram!(
            "handshake_duration_seconds",
            "direction" => self.direction,
            "outcome" => outcome::FAILURE
        )
        .record(self.start.elapsed().as_secs_f64());

        self.outcome_recorded = true;
    }
}

impl Drop for HandshakeMetrics {
    fn drop(&mut self) {
        if !self.outcome_recorded {
            counter!(
                "handshake_failure_total",
                "direction" => self.direction,
                "reason" => "unknown"
            )
            .increment(1);
        }
    }
}
