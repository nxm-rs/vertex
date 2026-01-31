//! Configuration for the scoring system.

use web_time::Duration;

/// Configuration for the scoring system.
#[derive(Debug, Clone)]
pub struct ScoreConfig {
    /// Half-life for score decay (default: 24 hours).
    pub decay_half_life: Duration,
    /// Minimum score before automatic ban (default: -100.0).
    pub ban_threshold: f64,
    /// Score below which peer is deprioritized (default: 0.0).
    pub deprioritize_threshold: f64,
    /// Maximum IPs tracked per overlay (default: 10).
    pub max_ips_per_overlay: usize,
    /// Maximum overlays tracked per IP (default: 50).
    pub max_overlays_per_ip: usize,
    /// Weight multipliers for different event types.
    pub weights: ScoreWeights,
}

impl Default for ScoreConfig {
    fn default() -> Self {
        Self {
            decay_half_life: Duration::from_secs(86400), // 24 hours
            ban_threshold: -100.0,
            deprioritize_threshold: 0.0,
            max_ips_per_overlay: 10,
            max_overlays_per_ip: 50,
            weights: ScoreWeights::default(),
        }
    }
}

impl ScoreConfig {
    /// Create config with custom ban threshold.
    pub fn with_ban_threshold(mut self, threshold: f64) -> Self {
        self.ban_threshold = threshold;
        self
    }

    /// Create config with custom decay half-life.
    pub fn with_decay_half_life(mut self, half_life: Duration) -> Self {
        self.decay_half_life = half_life;
        self
    }

    /// Create config with custom weights.
    pub fn with_weights(mut self, weights: ScoreWeights) -> Self {
        self.weights = weights;
        self
    }
}

/// Weight multipliers for score adjustments.
#[derive(Debug, Clone)]
pub struct ScoreWeights {
    // Positive events
    pub connection_success: f64,
    pub chunk_delivered: f64,
    pub chunk_delivered_fast: f64,
    pub protocol_compliance: f64,

    // Negative events (these are negative values)
    pub connection_timeout: f64,
    pub connection_refused: f64,
    pub protocol_error: f64,
    pub handshake_failure: f64,
    pub invalid_chunk: f64,
    pub slow_response: f64,
}

impl Default for ScoreWeights {
    fn default() -> Self {
        Self {
            // Positive
            connection_success: 1.0,
            chunk_delivered: 0.5,
            chunk_delivered_fast: 1.0,
            protocol_compliance: 0.1,
            // Negative
            connection_timeout: -2.0,
            connection_refused: -1.0,
            protocol_error: -10.0,
            handshake_failure: -5.0,
            invalid_chunk: -20.0,
            slow_response: -0.5,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ScoreConfig::default();
        assert_eq!(config.ban_threshold, -100.0);
        assert_eq!(config.deprioritize_threshold, 0.0);
        assert_eq!(config.max_ips_per_overlay, 10);
    }

    #[test]
    fn test_config_builder() {
        let config = ScoreConfig::default()
            .with_ban_threshold(-50.0)
            .with_decay_half_life(Duration::from_secs(3600));

        assert_eq!(config.ban_threshold, -50.0);
        assert_eq!(config.decay_half_life, Duration::from_secs(3600));
    }

    #[test]
    fn test_default_weights() {
        let weights = ScoreWeights::default();
        assert!(weights.connection_success > 0.0);
        assert!(weights.protocol_error < 0.0);
    }
}
