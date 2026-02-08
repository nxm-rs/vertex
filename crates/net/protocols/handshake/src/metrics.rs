//! Metrics for the handshake protocol.

use std::time::Instant;

use metrics::{counter, gauge, histogram};

use crate::{HandshakeError, codec::CodecError};

/// Label values for handshake metrics.
pub mod label {
    /// Direction of the handshake.
    pub mod direction {
        pub const INBOUND: &str = "inbound";
        pub const OUTBOUND: &str = "outbound";
    }

    /// Outcome of the handshake.
    pub mod outcome {
        pub const SUCCESS: &str = "success";
        pub const FAILURE: &str = "failure";
    }

    /// Type of peer.
    pub mod peer_type {
        pub const FULL: &str = "full";
        pub const LIGHT: &str = "light";
    }

    /// Failure reasons.
    pub mod reason {
        pub const PICKER_REJECTION: &str = "picker_rejection";
        pub const TIMEOUT: &str = "timeout";
        pub const NETWORK_ID_MISMATCH: &str = "network_id_mismatch";
        pub const MISSING_FIELD: &str = "missing_field";
        pub const FIELD_LENGTH_EXCEEDED: &str = "field_length_exceeded";
        pub const INVALID_DATA: &str = "invalid_data";
        pub const INVALID_MULTIADDR: &str = "invalid_multiaddr";
        pub const INVALID_SIGNATURE: &str = "invalid_signature";
        pub const INVALID_PEER: &str = "invalid_peer";
        pub const PROTOCOL_ERROR: &str = "protocol_error";
        pub const IO_ERROR: &str = "io_error";
        pub const STREAM_ERROR: &str = "stream_error";
        pub const CONNECTION_CLOSED: &str = "connection_closed";
        pub const MISSING_DATA: &str = "missing_data";
        pub const UNKNOWN: &str = "unknown";
    }
}

/// Tracks metrics for a single handshake operation.
///
/// On creation, increments active gauge and attempts counter.
/// On drop, decrements active gauge and records outcome.
pub struct HandshakeMetrics {
    direction: &'static str,
    start: Instant,
    outcome_recorded: bool,
}

impl HandshakeMetrics {
    /// Start tracking a new handshake.
    pub fn new(dir: &'static str) -> Self {
        counter!("handshake_attempts_total", "direction" => dir).increment(1);
        gauge!("handshake_active", "direction" => dir).increment(1.0);

        Self {
            direction: dir,
            start: Instant::now(),
            outcome_recorded: false,
        }
    }

    /// Record a successful handshake.
    pub fn record_success(mut self, full_node: bool) {
        let peer = if full_node {
            label::peer_type::FULL
        } else {
            label::peer_type::LIGHT
        };

        counter!("handshake_success_total", "direction" => self.direction, "peer_type" => peer)
            .increment(1);
        histogram!("handshake_duration_seconds", "direction" => self.direction, "outcome" => label::outcome::SUCCESS)
            .record(self.start.elapsed().as_secs_f64());

        self.outcome_recorded = true;
    }

    /// Record a failed handshake.
    pub fn record_failure(mut self, error: &HandshakeError) {
        let reason = error_reason(error);

        counter!("handshake_failure_total", "direction" => self.direction, "reason" => reason)
            .increment(1);
        histogram!("handshake_duration_seconds", "direction" => self.direction, "outcome" => label::outcome::FAILURE)
            .record(self.start.elapsed().as_secs_f64());

        self.outcome_recorded = true;
    }
}

impl Drop for HandshakeMetrics {
    fn drop(&mut self) {
        gauge!("handshake_active", "direction" => self.direction).decrement(1.0);

        if !self.outcome_recorded {
            counter!("handshake_failure_total", "direction" => self.direction, "reason" => label::reason::UNKNOWN)
                .increment(1);
        }
    }
}

fn error_reason(error: &HandshakeError) -> &'static str {
    match error {
        HandshakeError::PickerRejection => label::reason::PICKER_REJECTION,
        HandshakeError::Timeout => label::reason::TIMEOUT,
        HandshakeError::Codec(e) => codec_error_reason(e),
        HandshakeError::Protocol(_) => label::reason::PROTOCOL_ERROR,
        HandshakeError::Stream(_) => label::reason::STREAM_ERROR,
        HandshakeError::ConnectionClosed => label::reason::CONNECTION_CLOSED,
        HandshakeError::MissingData => label::reason::MISSING_DATA,
    }
}

fn codec_error_reason(error: &CodecError) -> &'static str {
    use crate::codec::HandshakeCodecDomainError;
    use vertex_net_codec::ProtocolCodecError;

    match error {
        ProtocolCodecError::Domain(domain) => match domain {
            HandshakeCodecDomainError::NetworkIdMismatch => label::reason::NETWORK_ID_MISMATCH,
            HandshakeCodecDomainError::MissingField(_) => label::reason::MISSING_FIELD,
            HandshakeCodecDomainError::FieldLengthExceeded(_, _, _) => label::reason::FIELD_LENGTH_EXCEEDED,
            HandshakeCodecDomainError::InvalidData(_) => label::reason::INVALID_DATA,
            HandshakeCodecDomainError::InvalidMultiaddr(_) => label::reason::INVALID_MULTIADDR,
            HandshakeCodecDomainError::InvalidSignature(_) => label::reason::INVALID_SIGNATURE,
            HandshakeCodecDomainError::InvalidPeer(_) => label::reason::INVALID_PEER,
        },
        ProtocolCodecError::Protocol(_) => label::reason::PROTOCOL_ERROR,
        ProtocolCodecError::Io(_) => label::reason::IO_ERROR,
    }
}
