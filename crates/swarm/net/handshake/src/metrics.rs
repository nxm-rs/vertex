//! Metrics and tracing for the handshake protocol.

use std::time::Instant;

use metrics::{counter, gauge, histogram};
use vertex_observability::{
    DURATION_FINE, DURATION_SECONDS, HistogramBucketConfig, LabelValue, StreamGuard,
    labels::{direction, outcome},
};
use vertex_swarm_peer::SwarmNodeType;

use crate::{HandshakeError, HandshakeInfo};

/// Histogram bucket configurations for handshake metrics.
///
/// Collect these at recorder install time via
/// [`vertex_observability::install_prometheus_recorder_with_buckets`].
pub const HISTOGRAM_BUCKETS: &[HistogramBucketConfig] = &[
    HistogramBucketConfig {
        suffix: "handshake_duration_seconds",
        buckets: DURATION_SECONDS,
    },
    HistogramBucketConfig {
        suffix: "stage_duration_seconds",
        buckets: DURATION_FINE,
    },
];

/// Handshake state machine stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum HandshakeStage {
    /// Connection established, handler created, handshake not started.
    Pending,
    /// Outbound: substream requested. Inbound: listening for SYN.
    Initiated,
    /// SYN message sent (outbound) or received (inbound).
    SynExchanged,
    /// SYNACK message sent (inbound) or received (outbound).
    SynAckExchanged,
    /// ACK sent/received, handshake complete.
    Completed,
    /// Handshake failed.
    Failed,
}

/// Tracks metrics for a single handshake operation with stage transitions.
pub struct HandshakeMetrics {
    direction: &'static str,
    purpose: &'static str,
    start: Instant,
    stage: HandshakeStage,
    stage_start: Instant,
    _stream: StreamGuard,
    outcome_recorded: bool,
}

impl HandshakeMetrics {
    /// Start tracking a new handshake.
    pub fn new(dir: &'static str, purpose: &'static str) -> Self {
        counter!("handshake_total", "direction" => dir, "purpose" => purpose).increment(1);
        gauge!("handshake_stage", "direction" => dir, "purpose" => purpose, "stage" => "pending")
            .increment(1.0);

        Self {
            direction: dir,
            purpose,
            start: Instant::now(),
            stage: HandshakeStage::Pending,
            stage_start: Instant::now(),
            _stream: StreamGuard::new("handshake", dir),
            outcome_recorded: false,
        }
    }

    /// Start tracking an inbound handshake.
    pub fn inbound(purpose: &'static str) -> Self {
        Self::new(direction::INBOUND, purpose)
    }

    /// Start tracking an outbound handshake.
    pub fn outbound(purpose: &'static str) -> Self {
        Self::new(direction::OUTBOUND, purpose)
    }

    /// Transition to a new stage, recording timing for the previous stage.
    pub fn transition_to(&mut self, new_stage: HandshakeStage) {
        if self.stage == new_stage {
            return;
        }

        let stage_duration = self.stage_start.elapsed();

        // Record stage duration
        histogram!(
            "handshake_stage_duration_seconds",
            "direction" => self.direction,
            "purpose" => self.purpose,
            "stage" => self.stage.label_value()
        )
        .record(stage_duration.as_secs_f64());

        // Decrement old stage gauge
        gauge!(
            "handshake_stage",
            "direction" => self.direction,
            "purpose" => self.purpose,
            "stage" => self.stage.label_value()
        )
        .decrement(1.0);

        // Increment new stage gauge (except for terminal states which use outcome metrics)
        if !matches!(
            new_stage,
            HandshakeStage::Completed | HandshakeStage::Failed
        ) {
            gauge!(
                "handshake_stage",
                "direction" => self.direction,
                "purpose" => self.purpose,
                "stage" => new_stage.label_value()
            )
            .increment(1.0);
        }

        self.stage = new_stage;
        self.stage_start = Instant::now();
    }

    /// Mark handshake as initiated (substream requested or listening).
    pub fn initiated(&mut self) {
        self.transition_to(HandshakeStage::Initiated);
    }

    /// Mark SYN as exchanged.
    pub fn syn_exchanged(&mut self) {
        self.transition_to(HandshakeStage::SynExchanged);
    }

    /// Mark SYNACK as exchanged.
    pub fn synack_exchanged(&mut self) {
        self.transition_to(HandshakeStage::SynAckExchanged);
    }

    /// Record the final handshake outcome, consuming the metrics tracker.
    ///
    /// On success, records success counter and duration histogram with node type.
    /// On failure, records failure counter with error reason and stage.
    pub fn record(mut self, result: &Result<HandshakeInfo, HandshakeError>) {
        match result {
            Ok(info) => {
                self.transition_to(HandshakeStage::Completed);

                // Map bootnode to client for metrics (bootnodes behave like clients)
                let node_type_label: &'static str = match info.node_type {
                    SwarmNodeType::Storer => SwarmNodeType::Storer.into(),
                    SwarmNodeType::Client | SwarmNodeType::Bootnode => SwarmNodeType::Client.into(),
                };

                counter!(
                    "handshake_success_total",
                    "direction" => self.direction,
                    "purpose" => self.purpose,
                    "node_type" => node_type_label
                )
                .increment(1);

                histogram!(
                    "handshake_duration_seconds",
                    "direction" => self.direction,
                    "purpose" => self.purpose,
                    "outcome" => outcome::SUCCESS,
                    "node_type" => node_type_label
                )
                .record(self.start.elapsed().as_secs_f64());
            }
            Err(error) => {
                // Capture the stage where failure occurred before transitioning.
                let failed_at = self.stage.label_value();
                self.transition_to(HandshakeStage::Failed);

                counter!(
                    "handshake_failure_total",
                    "direction" => self.direction,
                    "purpose" => self.purpose,
                    "reason" => error.label_value(),
                    "stage" => failed_at
                )
                .increment(1);

                histogram!(
                    "handshake_duration_seconds",
                    "direction" => self.direction,
                    "purpose" => self.purpose,
                    "outcome" => outcome::FAILURE
                )
                .record(self.start.elapsed().as_secs_f64());
            }
        }

        self.outcome_recorded = true;
    }

    /// Get total elapsed time since handshake started.
    pub fn elapsed(&self) -> std::time::Duration {
        self.start.elapsed()
    }

    /// Get current stage.
    pub fn stage(&self) -> HandshakeStage {
        self.stage
    }
}

impl Drop for HandshakeMetrics {
    fn drop(&mut self) {
        // Clean up stage gauge if not already transitioned to terminal state.
        if !matches!(
            self.stage,
            HandshakeStage::Completed | HandshakeStage::Failed
        ) {
            gauge!(
                "handshake_stage",
                "direction" => self.direction,
                "purpose" => self.purpose,
                "stage" => self.stage.label_value()
            )
            .decrement(1.0);
        }

        if !self.outcome_recorded {
            counter!(
                "handshake_failure_total",
                "direction" => self.direction,
                "purpose" => self.purpose,
                "reason" => "dropped",
                "stage" => self.stage.label_value()
            )
            .increment(1);

            histogram!(
                "handshake_duration_seconds",
                "direction" => self.direction,
                "purpose" => self.purpose,
                "outcome" => outcome::FAILURE
            )
            .record(self.start.elapsed().as_secs_f64());
        }
    }
}
