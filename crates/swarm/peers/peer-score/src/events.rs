//! Swarm-specific scoring events.

use std::time::Duration;

use vertex_net_peer_score::NetScoringEvent;

/// Swarm-specific peer scoring events.
#[derive(Debug, Clone)]
pub enum SwarmScoringEvent {
    /// Successful connection with optional latency.
    ConnectionSuccess { latency: Option<Duration> },

    /// Connection attempt timed out.
    ConnectionTimeout,

    /// Connection was refused by peer.
    ConnectionRefused,

    /// Handshake protocol failed.
    HandshakeFailure,

    /// Protocol-level error during communication.
    ProtocolError,

    /// Successful chunk retrieval.
    RetrievalSuccess { latency: Duration },

    /// Chunk retrieval failed.
    RetrievalFailure,

    /// Successful chunk push.
    PushSuccess { latency: Duration },

    /// Chunk push failed.
    PushFailure,

    /// Peer provided invalid data (chunk, signature, etc.).
    InvalidData,

    /// Peer is behaving maliciously.
    MaliciousBehavior,

    /// Bandwidth accounting violation.
    AccountingViolation,

    /// Peer exceeded rate limits.
    RateLimitExceeded,

    /// Successful ping/pong.
    PingSuccess { latency: Duration },

    /// Ping timed out.
    PingTimeout,

    /// Hive gossip received useful peers.
    GossipUseful,

    /// Hive gossip contained stale/invalid peers.
    GossipStale,
}

impl SwarmScoringEvent {
    /// Get the default weight for this event.
    ///
    /// Positive weights improve score, negative weights decrease it.
    /// These are default values; use [`SwarmScoringConfig`] for customization.
    pub fn default_weight(&self) -> f64 {
        match self {
            // Connection events
            Self::ConnectionSuccess { .. } => 1.0,
            Self::ConnectionTimeout => -1.5,
            Self::ConnectionRefused => -1.0,
            Self::HandshakeFailure => -5.0,
            Self::ProtocolError => -3.0,

            // Retrieval events
            Self::RetrievalSuccess { .. } => 0.5,
            Self::RetrievalFailure => -2.0,

            // Push events
            Self::PushSuccess { .. } => 0.5,
            Self::PushFailure => -2.0,

            // Data integrity events
            Self::InvalidData => -10.0,
            Self::MaliciousBehavior => -50.0,

            // Accounting events
            Self::AccountingViolation => -20.0,
            Self::RateLimitExceeded => -5.0,

            // Ping events
            Self::PingSuccess { .. } => 0.1,
            Self::PingTimeout => -0.5,

            // Gossip events
            Self::GossipUseful => 0.2,
            Self::GossipStale => -0.1,
        }
    }

    /// Extract latency if this event includes timing information.
    pub fn latency(&self) -> Option<Duration> {
        match self {
            Self::ConnectionSuccess { latency } => *latency,
            Self::RetrievalSuccess { latency }
            | Self::PushSuccess { latency }
            | Self::PingSuccess { latency } => Some(*latency),
            _ => None,
        }
    }

    /// Check if this is a connection success event.
    pub fn is_connection_success(&self) -> bool {
        matches!(self, Self::ConnectionSuccess { .. })
    }

    /// Check if this is a connection timeout event.
    pub fn is_connection_timeout(&self) -> bool {
        matches!(self, Self::ConnectionTimeout)
    }

    /// Check if this is a protocol error.
    pub fn is_protocol_error(&self) -> bool {
        matches!(self, Self::ProtocolError)
    }

    /// Check if this event should trigger a ban check.
    pub fn is_severe(&self) -> bool {
        matches!(
            self,
            Self::InvalidData | Self::MaliciousBehavior | Self::AccountingViolation
        )
    }
}

impl NetScoringEvent for SwarmScoringEvent {
    fn weight(&self) -> f64 {
        self.default_weight()
    }

    fn latency_ms(&self) -> Option<u32> {
        self.latency().map(|d| d.as_millis() as u32)
    }

    fn is_connection_success(&self) -> bool {
        SwarmScoringEvent::is_connection_success(self)
    }

    fn is_connection_timeout(&self) -> bool {
        SwarmScoringEvent::is_connection_timeout(self)
    }

    fn is_protocol_error(&self) -> bool {
        SwarmScoringEvent::is_protocol_error(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_weights() {
        // Positive events
        assert!(SwarmScoringEvent::ConnectionSuccess { latency: None }.default_weight() > 0.0);
        assert!(
            SwarmScoringEvent::RetrievalSuccess {
                latency: Duration::ZERO
            }
            .default_weight()
                > 0.0
        );
        assert!(SwarmScoringEvent::GossipUseful.default_weight() > 0.0);

        // Negative events
        assert!(SwarmScoringEvent::ConnectionTimeout.default_weight() < 0.0);
        assert!(SwarmScoringEvent::HandshakeFailure.default_weight() < 0.0);
        assert!(SwarmScoringEvent::MaliciousBehavior.default_weight() < -10.0);
    }

    #[test]
    fn test_latency_extraction() {
        let event = SwarmScoringEvent::ConnectionSuccess {
            latency: Some(Duration::from_millis(50)),
        };
        assert_eq!(event.latency(), Some(Duration::from_millis(50)));

        let event = SwarmScoringEvent::ConnectionTimeout;
        assert_eq!(event.latency(), None);
    }

    #[test]
    fn test_severe_events() {
        assert!(SwarmScoringEvent::MaliciousBehavior.is_severe());
        assert!(SwarmScoringEvent::InvalidData.is_severe());
        assert!(SwarmScoringEvent::AccountingViolation.is_severe());
        assert!(!SwarmScoringEvent::ConnectionTimeout.is_severe());
    }

    #[test]
    fn test_net_scoring_event_impl() {
        let event = SwarmScoringEvent::ConnectionSuccess {
            latency: Some(Duration::from_millis(100)),
        };
        assert!(event.weight() > 0.0);
        assert_eq!(event.latency_ms(), Some(100));
        assert!(NetScoringEvent::is_connection_success(&event));
    }
}
