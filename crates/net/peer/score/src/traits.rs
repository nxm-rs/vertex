//! Traits for scoring events and extensions.

use std::fmt::Debug;

use serde::{Deserialize, Serialize};

/// Scoring event with configurable weight (positive = good, negative = bad).
pub trait NetScoringEvent: Send + Sync + Debug + Clone + 'static {
    /// Weight to add to score (positive for good events, negative for bad).
    fn weight(&self) -> f64;

    /// Optional latency measurement in milliseconds.
    fn latency_ms(&self) -> Option<u32> {
        None
    }

    fn is_connection_success(&self) -> bool {
        false
    }

    fn is_connection_timeout(&self) -> bool {
        false
    }

    fn is_protocol_error(&self) -> bool {
        false
    }
}

/// Protocol-specific extended scoring state.
///
/// Implement this to add custom scoring metrics to `PeerScore`. The state is stored
/// alongside the generic scoring metrics and uses atomics or interior mutability.
pub trait NetPeerScoreExt: Debug + Default + Send + Sync + 'static {
    /// Serializable snapshot type for persistence.
    type Snapshot: Clone
        + Debug
        + Default
        + Send
        + Sync
        + Serialize
        + for<'de> Deserialize<'de>
        + 'static;

    fn snapshot(&self) -> Self::Snapshot;
    fn restore(&self, snapshot: &Self::Snapshot);
}

impl NetPeerScoreExt for () {
    type Snapshot = ();
    fn snapshot(&self) -> Self::Snapshot {}
    fn restore(&self, _snapshot: &Self::Snapshot) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone)]
    enum TestEvent {
        Success,
        Failure,
    }

    impl NetScoringEvent for TestEvent {
        fn weight(&self) -> f64 {
            match self {
                Self::Success => 1.0,
                Self::Failure => -1.0,
            }
        }
    }

    #[test]
    fn test_scoring_event() {
        assert!(TestEvent::Success.weight() > 0.0);
        assert!(TestEvent::Failure.weight() < 0.0);
    }
}
