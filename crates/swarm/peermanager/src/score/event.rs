//! Score events that affect peer and IP reputation.

/// Events that affect peer/IP scores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreEvent {
    /// Successful connection established.
    ConnectionSuccess,
    /// Connection attempt timed out.
    ConnectionTimeout,
    /// Connection was refused by the peer.
    ConnectionRefused,
    /// Handshake failed (identity mismatch, invalid signature, etc).
    HandshakeFailure,

    /// Peer followed protocol correctly.
    ProtocolCompliance,
    /// Peer violated protocol rules.
    ProtocolError,

    /// Chunk was delivered successfully.
    ChunkDelivered {
        /// Round-trip latency in milliseconds.
        latency_ms: u32,
    },
    /// Received invalid or corrupted chunk.
    InvalidChunk,
    /// Response was slower than expected threshold.
    SlowResponse,

    /// Manual positive adjustment by operator.
    ManualBoost(i32),
    /// Manual negative adjustment by operator.
    ManualPenalty(i32),
}

impl ScoreEvent {
    /// Returns true if this is a positive event.
    pub fn is_positive(&self) -> bool {
        matches!(
            self,
            ScoreEvent::ConnectionSuccess
                | ScoreEvent::ProtocolCompliance
                | ScoreEvent::ChunkDelivered { .. }
                | ScoreEvent::ManualBoost(_)
        )
    }

    /// Returns true if this is a negative event.
    pub fn is_negative(&self) -> bool {
        !self.is_positive()
    }

    /// Returns true if this is a connection-related event.
    pub fn is_connection_event(&self) -> bool {
        matches!(
            self,
            ScoreEvent::ConnectionSuccess
                | ScoreEvent::ConnectionTimeout
                | ScoreEvent::ConnectionRefused
                | ScoreEvent::HandshakeFailure
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_classification() {
        assert!(ScoreEvent::ConnectionSuccess.is_positive());
        assert!(ScoreEvent::ConnectionTimeout.is_negative());
        assert!(ScoreEvent::ChunkDelivered { latency_ms: 50 }.is_positive());
        assert!(ScoreEvent::InvalidChunk.is_negative());
        assert!(ScoreEvent::ManualBoost(10).is_positive());
        assert!(ScoreEvent::ManualPenalty(10).is_negative());
    }

    #[test]
    fn test_connection_events() {
        assert!(ScoreEvent::ConnectionSuccess.is_connection_event());
        assert!(ScoreEvent::ConnectionTimeout.is_connection_event());
        assert!(ScoreEvent::HandshakeFailure.is_connection_event());
        assert!(!ScoreEvent::ProtocolError.is_connection_event());
        assert!(!ScoreEvent::ChunkDelivered { latency_ms: 50 }.is_connection_event());
    }
}
