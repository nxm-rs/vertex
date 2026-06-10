//! Configurable scoring weights and builder.

use std::time::Duration;

use vertex_swarm_api::{DEFAULT_PEER_BAN_THRESHOLD, DEFAULT_PEER_WARN_THRESHOLD};

// The scoring event vocabulary is defined in `vertex-swarm-api`; this crate
// re-exports it so existing import paths keep working.
pub use vertex_swarm_api::SwarmScoringEvent;

scoring_events! {
    ConnectionSuccess { latency: Option<Duration> } => connection_success,
    ConnectionTimeout => connection_timeout,
    ConnectionRefused => connection_refused,
    HandshakeFailure => handshake_failure,
    ProtocolError => protocol_error,
    EarlyDisconnect { duration: Duration } => early_disconnect,
    RetrievalSuccess { latency: Duration } => retrieval_success,
    RetrievalFailure => retrieval_failure,
    PushSuccess { latency: Duration } => push_success,
    PushFailure => push_failure,
    InvalidData => invalid_data,
    MaliciousBehavior => malicious_behavior,
    AccountingViolation => accounting_violation,
    RateLimitExceeded => rate_limit_exceeded,
    PingSuccess { latency: Duration } => ping_success,
    PingTimeout => ping_timeout,
    GossipUseful => gossip_useful,
    GossipStale => gossip_stale,
    GossipVerified => gossip_verified,
    GossipInvalid => gossip_invalid,
    GossipUnreachable => gossip_unreachable;

    ban_threshold = DEFAULT_PEER_BAN_THRESHOLD,
    warn_threshold = DEFAULT_PEER_WARN_THRESHOLD,
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
    fn test_default_weights_match_events() {
        let config = SwarmScoringConfig::default();
        let events = [
            SwarmScoringEvent::ConnectionSuccess { latency: None },
            SwarmScoringEvent::ConnectionTimeout,
            SwarmScoringEvent::HandshakeFailure,
            SwarmScoringEvent::EarlyDisconnect {
                duration: Duration::ZERO,
            },
            SwarmScoringEvent::RetrievalSuccess {
                latency: Duration::ZERO,
            },
            SwarmScoringEvent::MaliciousBehavior,
            SwarmScoringEvent::GossipUnreachable,
        ];
        for event in events {
            assert!(
                (config.weight_for(&event) - event.default_weight()).abs() < f64::EPSILON,
                "default config weight diverges from default_weight for {event:?}"
            );
        }
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
}
