//! Scoring policy abstraction.

/// Scoring policy that determines score adjustments for events.
pub trait ScoringPolicy: Send + Sync {
    /// Score adjustment for successful connection.
    fn on_success(&self) -> f64 {
        1.0
    }

    /// Score adjustment for connection timeout.
    fn on_timeout(&self) -> f64 {
        -1.5
    }

    /// Score adjustment for connection refusal.
    fn on_refusal(&self) -> f64 {
        -1.0
    }

    /// Score adjustment for handshake failure.
    fn on_handshake_failure(&self) -> f64 {
        -5.0
    }

    /// Score adjustment for protocol error.
    fn on_protocol_error(&self) -> f64 {
        -3.0
    }

    /// Score threshold below which a peer should be banned.
    fn ban_threshold(&self) -> f64 {
        -100.0
    }
}

/// Default scoring policy with standard weights.
#[derive(Debug, Clone, Default)]
pub struct DefaultScoringPolicy;

impl ScoringPolicy for DefaultScoringPolicy {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_policy() {
        let policy = DefaultScoringPolicy;
        assert!(policy.on_success() > 0.0);
        assert!(policy.on_timeout() < 0.0);
        assert!(policy.on_refusal() < 0.0);
        assert!(policy.on_handshake_failure() < 0.0);
        assert!(policy.on_protocol_error() < 0.0);
        assert!(policy.ban_threshold() < 0.0);
    }
}
