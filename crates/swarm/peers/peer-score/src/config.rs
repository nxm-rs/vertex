//! Configurable scoring weights.

use serde::{Deserialize, Serialize};
use vertex_net_peer_score::ScoringPolicy;

/// Configuration for Swarm peer scoring weights.
///
/// All weights can be customized. Positive values improve score,
/// negative values decrease it. Use [`SwarmScoringConfigBuilder`] for
/// ergonomic configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmScoringConfig {
    connection_success: f64,
    connection_timeout: f64,
    connection_refused: f64,
    handshake_failure: f64,
    protocol_error: f64,
    retrieval_success: f64,
    retrieval_failure: f64,
    push_success: f64,
    push_failure: f64,
    invalid_data: f64,
    malicious_behavior: f64,
    accounting_violation: f64,
    rate_limit_exceeded: f64,
    ping_success: f64,
    ping_timeout: f64,
    gossip_useful: f64,
    gossip_stale: f64,
    ban_threshold: f64,
    warn_threshold: f64,
}

impl Default for SwarmScoringConfig {
    fn default() -> Self {
        Self {
            connection_success: 1.0,
            connection_timeout: -1.5,
            connection_refused: -1.0,
            handshake_failure: -5.0,
            protocol_error: -3.0,
            retrieval_success: 0.5,
            retrieval_failure: -2.0,
            push_success: 0.5,
            push_failure: -2.0,
            invalid_data: -10.0,
            malicious_behavior: -50.0,
            accounting_violation: -20.0,
            rate_limit_exceeded: -5.0,
            ping_success: 0.1,
            ping_timeout: -0.5,
            gossip_useful: 0.2,
            gossip_stale: -0.1,
            ban_threshold: -100.0,
            warn_threshold: -50.0,
        }
    }
}

impl SwarmScoringConfig {
    /// Create a new config with default values.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a builder for custom configuration.
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
            .invalid_data(-25.0)
            .malicious_behavior(-100.0)
            .ban_threshold(-50.0)
            .build()
    }

    // Getters for all fields

    #[must_use]
    pub fn connection_success(&self) -> f64 {
        self.connection_success
    }

    #[must_use]
    pub fn connection_timeout(&self) -> f64 {
        self.connection_timeout
    }

    #[must_use]
    pub fn connection_refused(&self) -> f64 {
        self.connection_refused
    }

    #[must_use]
    pub fn handshake_failure(&self) -> f64 {
        self.handshake_failure
    }

    #[must_use]
    pub fn protocol_error(&self) -> f64 {
        self.protocol_error
    }

    #[must_use]
    pub fn retrieval_success(&self) -> f64 {
        self.retrieval_success
    }

    #[must_use]
    pub fn retrieval_failure(&self) -> f64 {
        self.retrieval_failure
    }

    #[must_use]
    pub fn push_success(&self) -> f64 {
        self.push_success
    }

    #[must_use]
    pub fn push_failure(&self) -> f64 {
        self.push_failure
    }

    #[must_use]
    pub fn invalid_data(&self) -> f64 {
        self.invalid_data
    }

    #[must_use]
    pub fn malicious_behavior(&self) -> f64 {
        self.malicious_behavior
    }

    #[must_use]
    pub fn accounting_violation(&self) -> f64 {
        self.accounting_violation
    }

    #[must_use]
    pub fn rate_limit_exceeded(&self) -> f64 {
        self.rate_limit_exceeded
    }

    #[must_use]
    pub fn ping_success(&self) -> f64 {
        self.ping_success
    }

    #[must_use]
    pub fn ping_timeout(&self) -> f64 {
        self.ping_timeout
    }

    #[must_use]
    pub fn gossip_useful(&self) -> f64 {
        self.gossip_useful
    }

    #[must_use]
    pub fn gossip_stale(&self) -> f64 {
        self.gossip_stale
    }

    #[must_use]
    pub fn ban_threshold(&self) -> f64 {
        self.ban_threshold
    }

    #[must_use]
    pub fn warn_threshold(&self) -> f64 {
        self.warn_threshold
    }

    /// Check if a score should trigger a ban.
    #[must_use]
    pub fn should_ban(&self, score: f64) -> bool {
        score < self.ban_threshold
    }

    /// Check if a score is in warning territory.
    #[must_use]
    pub fn should_warn(&self, score: f64) -> bool {
        score < self.warn_threshold && score >= self.ban_threshold
    }

    /// Get weight for a specific event type.
    #[must_use]
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

/// Builder for [`SwarmScoringConfig`] with fluent API.
#[derive(Debug, Clone)]
pub struct SwarmScoringConfigBuilder {
    config: SwarmScoringConfig,
}

impl Default for SwarmScoringConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl SwarmScoringConfigBuilder {
    /// Create a new builder with default values.
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: SwarmScoringConfig::default(),
        }
    }

    /// Build the configuration.
    #[must_use]
    pub fn build(self) -> SwarmScoringConfig {
        self.config
    }

    /// Set connection success weight.
    #[must_use]
    pub fn connection_success(mut self, value: f64) -> Self {
        self.config.connection_success = value;
        self
    }

    /// Set connection timeout penalty.
    #[must_use]
    pub fn connection_timeout(mut self, value: f64) -> Self {
        self.config.connection_timeout = value;
        self
    }

    /// Set connection refused penalty.
    #[must_use]
    pub fn connection_refused(mut self, value: f64) -> Self {
        self.config.connection_refused = value;
        self
    }

    /// Set handshake failure penalty.
    #[must_use]
    pub fn handshake_failure(mut self, value: f64) -> Self {
        self.config.handshake_failure = value;
        self
    }

    /// Set protocol error penalty.
    #[must_use]
    pub fn protocol_error(mut self, value: f64) -> Self {
        self.config.protocol_error = value;
        self
    }

    /// Set retrieval success weight.
    #[must_use]
    pub fn retrieval_success(mut self, value: f64) -> Self {
        self.config.retrieval_success = value;
        self
    }

    /// Set retrieval failure penalty.
    #[must_use]
    pub fn retrieval_failure(mut self, value: f64) -> Self {
        self.config.retrieval_failure = value;
        self
    }

    /// Set push success weight.
    #[must_use]
    pub fn push_success(mut self, value: f64) -> Self {
        self.config.push_success = value;
        self
    }

    /// Set push failure penalty.
    #[must_use]
    pub fn push_failure(mut self, value: f64) -> Self {
        self.config.push_failure = value;
        self
    }

    /// Set invalid data penalty.
    #[must_use]
    pub fn invalid_data(mut self, value: f64) -> Self {
        self.config.invalid_data = value;
        self
    }

    /// Set malicious behavior penalty.
    #[must_use]
    pub fn malicious_behavior(mut self, value: f64) -> Self {
        self.config.malicious_behavior = value;
        self
    }

    /// Set accounting violation penalty.
    #[must_use]
    pub fn accounting_violation(mut self, value: f64) -> Self {
        self.config.accounting_violation = value;
        self
    }

    /// Set rate limit exceeded penalty.
    #[must_use]
    pub fn rate_limit_exceeded(mut self, value: f64) -> Self {
        self.config.rate_limit_exceeded = value;
        self
    }

    /// Set ping success weight.
    #[must_use]
    pub fn ping_success(mut self, value: f64) -> Self {
        self.config.ping_success = value;
        self
    }

    /// Set ping timeout penalty.
    #[must_use]
    pub fn ping_timeout(mut self, value: f64) -> Self {
        self.config.ping_timeout = value;
        self
    }

    /// Set gossip useful weight.
    #[must_use]
    pub fn gossip_useful(mut self, value: f64) -> Self {
        self.config.gossip_useful = value;
        self
    }

    /// Set gossip stale penalty.
    #[must_use]
    pub fn gossip_stale(mut self, value: f64) -> Self {
        self.config.gossip_stale = value;
        self
    }

    /// Set ban threshold.
    #[must_use]
    pub fn ban_threshold(mut self, value: f64) -> Self {
        self.config.ban_threshold = value;
        self
    }

    /// Set warning threshold.
    #[must_use]
    pub fn warn_threshold(mut self, value: f64) -> Self {
        self.config.warn_threshold = value;
        self
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
        // Other fields should have defaults
        assert!((config.connection_timeout() - -1.5).abs() < 0.001);
    }

    #[test]
    fn test_lenient_vs_strict() {
        let lenient = SwarmScoringConfig::lenient();
        let strict = SwarmScoringConfig::strict();

        // Lenient should have smaller penalties
        assert!(lenient.connection_timeout().abs() < strict.connection_timeout().abs());
        assert!(lenient.handshake_failure().abs() < strict.handshake_failure().abs());

        // Lenient should have higher (less negative) ban threshold
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
        assert!(!config.should_warn(-101.0)); // Below ban threshold
    }

    #[test]
    fn test_scoring_policy_impl() {
        let config = SwarmScoringConfig::default();
        assert_eq!(ScoringPolicy::on_success(&config), config.connection_success());
        assert_eq!(ScoringPolicy::on_timeout(&config), config.connection_timeout());
        assert_eq!(ScoringPolicy::ban_threshold(&config), config.ban_threshold());
    }

    #[test]
    fn test_serialization() {
        let config = SwarmScoringConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let restored: SwarmScoringConfig = serde_json::from_str(&json).unwrap();
        assert!((config.connection_success() - restored.connection_success()).abs() < 0.001);
    }
}
