//! Scoring events, configurable weights, and builder.

use std::time::Duration;

use vertex_swarm_api::{DEFAULT_PEER_BAN_THRESHOLD, DEFAULT_PEER_WARN_THRESHOLD};

scoring_events! {
    /// Successful connection with optional latency.
    ConnectionSuccess { latency: Option<Duration> }
        => connection_success = 1.0,
    /// Connection attempt timed out.
    ConnectionTimeout
        => connection_timeout = -1.5,
    /// Connection was refused by peer.
    ConnectionRefused
        => connection_refused = -1.0,
    /// Handshake protocol failed.
    HandshakeFailure
        => handshake_failure = -5.0,
    /// Protocol-level error during communication.
    ProtocolError
        => protocol_error = -3.0,
    /// Peer disconnected shortly after completing handshake (connection instability).
    EarlyDisconnect { duration: Duration }
        => early_disconnect = -3.0,
    /// Successful chunk retrieval.
    RetrievalSuccess { latency: Duration }
        => retrieval_success = 0.5,
    /// Chunk retrieval failed.
    RetrievalFailure
        => retrieval_failure = -2.0,
    /// Successful chunk push.
    PushSuccess { latency: Duration }
        => push_success = 0.5,
    /// Chunk push failed.
    PushFailure
        => push_failure = -2.0,
    /// Peer provided invalid data (chunk, signature, etc.).
    InvalidData
        => invalid_data = -10.0,
    /// Peer is behaving maliciously.
    MaliciousBehavior
        => malicious_behavior = -50.0,
    /// Bandwidth accounting violation.
    AccountingViolation
        => accounting_violation = -20.0,
    /// Peer exceeded rate limits.
    RateLimitExceeded
        => rate_limit_exceeded = -5.0,
    /// Successful ping/pong.
    PingSuccess { latency: Duration }
        => ping_success = 0.1,
    /// Ping timed out.
    PingTimeout
        => ping_timeout = -0.5,
    /// Hive gossip received useful peers.
    GossipUseful
        => gossip_useful = 0.2,
    /// Hive gossip contained stale/invalid peers.
    GossipStale
        => gossip_stale = -0.1,
    /// Gossiped peer was verified via handshake (signature, overlay, multiaddr all match).
    GossipVerified
        => gossip_verified = 1.0,
    /// Gossiped peer failed verification (overlay, signature, or multiaddr mismatch).
    GossipInvalid
        => gossip_invalid = -15.0,
    /// Gossiped peer could not be reached for verification.
    GossipUnreachable
        => gossip_unreachable = -0.5;

    ban_threshold = DEFAULT_PEER_BAN_THRESHOLD,
    warn_threshold = DEFAULT_PEER_WARN_THRESHOLD,
}

impl SwarmScoringEvent {
    /// Extract latency if this event includes timing information.
    #[must_use]
    pub fn latency(&self) -> Option<Duration> {
        match self {
            Self::ConnectionSuccess { latency } => *latency,
            Self::RetrievalSuccess { latency }
            | Self::PushSuccess { latency }
            | Self::PingSuccess { latency } => Some(*latency),
            _ => None,
        }
    }

    pub fn is_connection_success(&self) -> bool {
        matches!(self, Self::ConnectionSuccess { .. })
    }

    pub fn is_connection_timeout(&self) -> bool {
        matches!(self, Self::ConnectionTimeout)
    }

    pub fn is_protocol_error(&self) -> bool {
        matches!(self, Self::ProtocolError)
    }

    /// True for events that should trigger an immediate ban check.
    pub fn is_severe(&self) -> bool {
        matches!(
            self,
            Self::InvalidData
                | Self::MaliciousBehavior
                | Self::AccountingViolation
                | Self::GossipInvalid
        )
    }
}

impl SwarmScoringConfig {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn builder() -> SwarmScoringConfigBuilder {
        SwarmScoringConfigBuilder::new()
    }

    /// Create a lenient config with reduced penalties.
    #[must_use]
    pub fn lenient() -> Self {
        Self::builder()
            .connection_timeout(-0.5)
            .connection_refused(-0.3)
            .handshake_failure(-2.0)
            .protocol_error(-1.0)
            .early_disconnect(-1.5)
            .retrieval_failure(-0.5)
            .push_failure(-0.5)
            .ban_threshold(-200.0)
            .build()
    }

    /// Create a strict config with increased penalties.
    #[must_use]
    pub fn strict() -> Self {
        Self::builder()
            .connection_timeout(-3.0)
            .connection_refused(-2.0)
            .handshake_failure(-10.0)
            .protocol_error(-5.0)
            .early_disconnect(-5.0)
            .invalid_data(-25.0)
            .malicious_behavior(-100.0)
            .ban_threshold(-50.0)
            .build()
    }

    #[must_use]
    pub fn should_ban(&self, score: f64) -> bool {
        score < self.ban_threshold
    }

    /// True if score is below warning threshold but above ban threshold.
    #[must_use]
    pub fn should_warn(&self, score: f64) -> bool {
        score < self.warn_threshold && score >= self.ban_threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = SwarmScoringConfig::default();
        assert!(config.connection_success() > 0.0);
        assert!(config.connection_timeout() < 0.0);
        assert!(config.ban_threshold() < 0.0);
    }

    #[test]
    fn test_builder() {
        let config = SwarmScoringConfig::builder()
            .connection_success(2.0)
            .ban_threshold(-150.0)
            .build();

        assert!((config.connection_success() - 2.0).abs() < 0.001);
        assert!((config.ban_threshold() - -150.0).abs() < 0.001);
        assert!((config.connection_timeout() - -1.5).abs() < 0.001);
    }

    #[test]
    fn test_lenient_vs_strict() {
        let lenient = SwarmScoringConfig::lenient();
        let strict = SwarmScoringConfig::strict();

        assert!(lenient.connection_timeout().abs() < strict.connection_timeout().abs());
        assert!(lenient.handshake_failure().abs() < strict.handshake_failure().abs());
        assert!(lenient.ban_threshold() < strict.ban_threshold());
    }

    #[test]
    fn test_should_ban() {
        let config = SwarmScoringConfig::default();
        assert!(!config.should_ban(0.0));
        assert!(!config.should_ban(-50.0));
        assert!(config.should_ban(-101.0));
    }

    #[test]
    fn test_should_warn() {
        let config = SwarmScoringConfig::default();
        assert!(!config.should_warn(0.0));
        assert!(config.should_warn(-60.0));
        assert!(!config.should_warn(-101.0));
    }

    #[test]
    fn test_serialization() {
        let config = SwarmScoringConfig::default();
        let bytes = postcard::to_allocvec(&config).unwrap();
        let restored: SwarmScoringConfig = postcard::from_bytes(&bytes).unwrap();
        assert!((config.connection_success() - restored.connection_success()).abs() < 0.001);
    }

    #[test]
    fn test_event_weights() {
        assert!(SwarmScoringEvent::ConnectionSuccess { latency: None }.default_weight() > 0.0);
        assert!(
            SwarmScoringEvent::RetrievalSuccess {
                latency: Duration::ZERO
            }
            .default_weight()
                > 0.0
        );
        assert!(SwarmScoringEvent::GossipUseful.default_weight() > 0.0);

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
}
