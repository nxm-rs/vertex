//! Configurable scoring weights.

use serde::{Deserialize, Serialize};
use vertex_net_peer_score::ScoringPolicy;

/// Configuration for Swarm peer scoring weights.
///
/// All weights can be customized. Positive values improve score,
/// negative values decrease it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmScoringConfig {
    // Connection events
    pub connection_success: f64,
    pub connection_timeout: f64,
    pub connection_refused: f64,
    pub handshake_failure: f64,
    pub protocol_error: f64,

    // Data transfer events
    pub retrieval_success: f64,
    pub retrieval_failure: f64,
    pub push_success: f64,
    pub push_failure: f64,

    // Data integrity events
    pub invalid_data: f64,
    pub malicious_behavior: f64,

    // Accounting events
    pub accounting_violation: f64,
    pub rate_limit_exceeded: f64,

    // Ping events
    pub ping_success: f64,
    pub ping_timeout: f64,

    // Gossip events
    pub gossip_useful: f64,
    pub gossip_stale: f64,

    // Thresholds
    pub ban_threshold: f64,
    pub warn_threshold: f64,
}

impl Default for SwarmScoringConfig {
    fn default() -> Self {
        Self {
            // Connection events
            connection_success: 1.0,
            connection_timeout: -1.5,
            connection_refused: -1.0,
            handshake_failure: -5.0,
            protocol_error: -3.0,

            // Data transfer events
            retrieval_success: 0.5,
            retrieval_failure: -2.0,
            push_success: 0.5,
            push_failure: -2.0,

            // Data integrity events
            invalid_data: -10.0,
            malicious_behavior: -50.0,

            // Accounting events
            accounting_violation: -20.0,
            rate_limit_exceeded: -5.0,

            // Ping events
            ping_success: 0.1,
            ping_timeout: -0.5,

            // Gossip events
            gossip_useful: 0.2,
            gossip_stale: -0.1,

            // Thresholds
            ban_threshold: -100.0,
            warn_threshold: -50.0,
        }
    }
}

impl SwarmScoringConfig {
    /// Create a new config with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a lenient config with reduced penalties.
    pub fn lenient() -> Self {
        Self {
            connection_timeout: -0.5,
            connection_refused: -0.3,
            handshake_failure: -2.0,
            protocol_error: -1.0,
            retrieval_failure: -0.5,
            push_failure: -0.5,
            ban_threshold: -200.0,
            ..Self::default()
        }
    }

    /// Create a strict config with increased penalties.
    pub fn strict() -> Self {
        Self {
            connection_timeout: -3.0,
            connection_refused: -2.0,
            handshake_failure: -10.0,
            protocol_error: -5.0,
            invalid_data: -25.0,
            malicious_behavior: -100.0,
            ban_threshold: -50.0,
            ..Self::default()
        }
    }

    /// Check if a score should trigger a ban.
    pub fn should_ban(&self, score: f64) -> bool {
        score < self.ban_threshold
    }

    /// Check if a score is in warning territory.
    pub fn should_warn(&self, score: f64) -> bool {
        score < self.warn_threshold && score >= self.ban_threshold
    }

    /// Get weight for a specific event type.
    pub fn weight_for(&self, event: &super::SwarmScoringEvent) -> f64 {
        use super::SwarmScoringEvent::*;
        match event {
            ConnectionSuccess { .. } => self.connection_success,
            ConnectionTimeout => self.connection_timeout,
            ConnectionRefused => self.connection_refused,
            HandshakeFailure => self.handshake_failure,
            ProtocolError => self.protocol_error,
            RetrievalSuccess { .. } => self.retrieval_success,
            RetrievalFailure => self.retrieval_failure,
            PushSuccess { .. } => self.push_success,
            PushFailure => self.push_failure,
            InvalidData => self.invalid_data,
            MaliciousBehavior => self.malicious_behavior,
            AccountingViolation => self.accounting_violation,
            RateLimitExceeded => self.rate_limit_exceeded,
            PingSuccess { .. } => self.ping_success,
            PingTimeout => self.ping_timeout,
            GossipUseful => self.gossip_useful,
            GossipStale => self.gossip_stale,
        }
    }
}

impl ScoringPolicy for SwarmScoringConfig {
    fn on_success(&self) -> f64 {
        self.connection_success
    }

    fn on_timeout(&self) -> f64 {
        self.connection_timeout
    }

    fn on_refusal(&self) -> f64 {
        self.connection_refused
    }

    fn on_handshake_failure(&self) -> f64 {
        self.handshake_failure
    }

    fn on_protocol_error(&self) -> f64 {
        self.protocol_error
    }

    fn ban_threshold(&self) -> f64 {
        self.ban_threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = SwarmScoringConfig::default();
        assert!(config.connection_success > 0.0);
        assert!(config.connection_timeout < 0.0);
        assert!(config.ban_threshold < 0.0);
    }

    #[test]
    fn test_lenient_vs_strict() {
        let lenient = SwarmScoringConfig::lenient();
        let strict = SwarmScoringConfig::strict();

        // Lenient should have smaller penalties
        assert!(lenient.connection_timeout.abs() < strict.connection_timeout.abs());
        assert!(lenient.handshake_failure.abs() < strict.handshake_failure.abs());

        // Lenient should have higher (less negative) ban threshold
        assert!(lenient.ban_threshold < strict.ban_threshold);
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
        assert!(!config.should_warn(-101.0)); // Below ban threshold
    }

    #[test]
    fn test_scoring_policy_impl() {
        let config = SwarmScoringConfig::default();
        assert_eq!(ScoringPolicy::on_success(&config), config.connection_success);
        assert_eq!(ScoringPolicy::on_timeout(&config), config.connection_timeout);
        assert_eq!(ScoringPolicy::ban_threshold(&config), config.ban_threshold);
    }

    #[test]
    fn test_serialization() {
        let config = SwarmScoringConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let restored: SwarmScoringConfig = serde_json::from_str(&json).unwrap();
        assert!((config.connection_success - restored.connection_success).abs() < 0.001);
    }
}
