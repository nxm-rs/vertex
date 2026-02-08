//! Metrics for the hive protocol.

use std::time::Instant;

use metrics::{counter, gauge, histogram};

use crate::codec::HiveCodecError;

/// Label values for hive metrics.
pub mod label {
    /// Direction of the exchange.
    pub mod direction {
        pub const INBOUND: &str = "inbound";
        pub const OUTBOUND: &str = "outbound";
    }

    /// Outcome of the exchange.
    pub mod outcome {
        pub const SUCCESS: &str = "success";
        pub const FAILURE: &str = "failure";
    }

    /// Peer validation outcomes.
    pub mod peer_outcome {
        pub const VALID: &str = "valid";
        pub const INVALID: &str = "invalid";
    }

    /// Peer validation failure reasons.
    pub mod validation {
        pub const OVERLAY_LENGTH: &str = "overlay_length";
        pub const SIGNATURE_FORMAT: &str = "signature_format";
        pub const NONCE_LENGTH: &str = "nonce_length";
        pub const PEER_VALIDATION: &str = "peer_validation";
        /// Rejected because peer is our own overlay (self-dial prevention).
        pub const SELF_OVERLAY: &str = "self_overlay";
    }

    /// Exchange error reasons.
    pub mod error {
        pub const IO: &str = "io";
        pub const CODEC: &str = "codec";
        pub const TIMEOUT: &str = "timeout";
        pub const UNKNOWN: &str = "unknown";
    }
}

/// Tracks metrics for a single hive exchange.
pub struct HiveMetrics {
    direction: &'static str,
    start: Instant,
    peers_valid: u64,
    peers_invalid: u64,
    outcome_recorded: bool,
}

impl HiveMetrics {
    /// Start tracking a new hive exchange.
    pub fn new(dir: &'static str) -> Self {
        counter!("hive_exchanges_total", "direction" => dir).increment(1);
        gauge!("hive_exchanges_active", "direction" => dir).increment(1.0);

        Self {
            direction: dir,
            start: Instant::now(),
            peers_valid: 0,
            peers_invalid: 0,
            outcome_recorded: false,
        }
    }

    /// Record a peer validation failure.
    pub fn record_validation_failure(&mut self, reason: &'static str) {
        counter!("hive_peer_validation_failures_total", "reason" => reason).increment(1);
        self.peers_invalid += 1;
    }

    /// Record successfully validated/sent peers.
    pub fn add_valid_peers(&mut self, count: u64) {
        self.peers_valid += count;
    }

    /// Record a successful exchange.
    pub fn record_success(mut self) {
        // Record peer counts
        if self.direction == label::direction::INBOUND {
            counter!("hive_peers_received_total", "outcome" => label::peer_outcome::VALID)
                .increment(self.peers_valid);
            counter!("hive_peers_received_total", "outcome" => label::peer_outcome::INVALID)
                .increment(self.peers_invalid);
        } else {
            counter!("hive_peers_sent_total").increment(self.peers_valid);
        }

        // Record exchange outcome
        counter!("hive_exchange_outcomes_total", "direction" => self.direction, "outcome" => label::outcome::SUCCESS)
            .increment(1);
        histogram!("hive_exchange_duration_seconds", "direction" => self.direction, "outcome" => label::outcome::SUCCESS)
            .record(self.start.elapsed().as_secs_f64());
        histogram!("hive_peers_per_exchange", "direction" => self.direction)
            .record(self.peers_valid as f64);

        self.outcome_recorded = true;
    }

    /// Record a failed exchange.
    fn record_failure_with_reason(mut self, reason: &'static str) {
        counter!("hive_exchange_outcomes_total", "direction" => self.direction, "outcome" => label::outcome::FAILURE)
            .increment(1);
        counter!("hive_errors_total", "direction" => self.direction, "reason" => reason)
            .increment(1);
        histogram!("hive_exchange_duration_seconds", "direction" => self.direction, "outcome" => label::outcome::FAILURE)
            .record(self.start.elapsed().as_secs_f64());

        self.outcome_recorded = true;
    }

    /// Record a codec error failure.
    pub fn record_codec_error(self, err: &HiveCodecError) {
        use vertex_net_codec::ProtocolCodecError;
        let reason = match err {
            ProtocolCodecError::Io(_) => label::error::IO,
            ProtocolCodecError::Protocol(_) | ProtocolCodecError::Domain(_) => label::error::CODEC,
        };
        self.record_failure_with_reason(reason);
    }
}

impl Drop for HiveMetrics {
    fn drop(&mut self) {
        gauge!("hive_exchanges_active", "direction" => self.direction).decrement(1.0);

        if !self.outcome_recorded {
            counter!("hive_exchange_outcomes_total", "direction" => self.direction, "outcome" => label::outcome::FAILURE)
                .increment(1);
            counter!("hive_errors_total", "direction" => self.direction, "reason" => label::error::UNKNOWN)
                .increment(1);
        }
    }
}
