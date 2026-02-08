//! Callback traits for score change notifications.

use auto_impl::auto_impl;
use vertex_swarm_primitives::OverlayAddress;

use crate::SwarmScoringEvent;

/// Observer trait for peer score changes.
///
/// Implement this trait to receive notifications when peer scores change.
/// This enables reactive behavior like triggering disconnection when
/// scores drop below thresholds.
#[auto_impl(&, Box, Arc)]
pub trait ScoreObserver: Send + Sync {
    /// Called when a peer's score changes.
    ///
    /// `old_score` is the score before the event, `new_score` is after.
    fn on_score_changed(
        &self,
        overlay: &OverlayAddress,
        old_score: f64,
        new_score: f64,
        event: &SwarmScoringEvent,
    );

    /// Called when a peer's score crosses the warning threshold.
    ///
    /// This is called once when the score first drops below the threshold,
    /// not on every score change while below.
    fn on_score_warning(&self, overlay: &OverlayAddress, score: f64) {
        let _ = (overlay, score);
    }

    /// Called when a peer should be banned based on score.
    ///
    /// The implementation should trigger disconnection and prevent
    /// future connections to this peer.
    fn on_should_ban(&self, overlay: &OverlayAddress, score: f64, reason: &str) {
        let _ = (overlay, score, reason);
    }

    /// Called when a severe event occurs that may require immediate action.
    ///
    /// Severe events include malicious behavior, invalid data, or accounting
    /// violations. The observer may want to take action even if the score
    /// hasn't crossed the ban threshold.
    fn on_severe_event(&self, overlay: &OverlayAddress, event: &SwarmScoringEvent) {
        let _ = (overlay, event);
    }
}

/// No-op implementation of ScoreObserver.
///
/// Use this when you don't need score change notifications.
#[derive(Debug, Clone, Default)]
pub struct NoOpScoreObserver;

impl ScoreObserver for NoOpScoreObserver {
    fn on_score_changed(
        &self,
        _overlay: &OverlayAddress,
        _old_score: f64,
        _new_score: f64,
        _event: &SwarmScoringEvent,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    struct CountingObserver {
        changes: AtomicU32,
        warnings: AtomicU32,
        bans: AtomicU32,
    }

    impl CountingObserver {
        fn new() -> Self {
            Self {
                changes: AtomicU32::new(0),
                warnings: AtomicU32::new(0),
                bans: AtomicU32::new(0),
            }
        }
    }

    impl ScoreObserver for CountingObserver {
        fn on_score_changed(
            &self,
            _overlay: &OverlayAddress,
            _old_score: f64,
            _new_score: f64,
            _event: &SwarmScoringEvent,
        ) {
            self.changes.fetch_add(1, Ordering::Relaxed);
        }

        fn on_score_warning(&self, _overlay: &OverlayAddress, _score: f64) {
            self.warnings.fetch_add(1, Ordering::Relaxed);
        }

        fn on_should_ban(&self, _overlay: &OverlayAddress, _score: f64, _reason: &str) {
            self.bans.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn test_overlay() -> OverlayAddress {
        OverlayAddress::from([1u8; 32])
    }

    #[test]
    fn test_no_op_observer() {
        let observer = NoOpScoreObserver;
        let overlay = test_overlay();
        let event = SwarmScoringEvent::ConnectionSuccess {
            latency: Some(Duration::from_millis(50)),
        };

        // Should not panic
        observer.on_score_changed(&overlay, 0.0, 1.0, &event);
        observer.on_score_warning(&overlay, -60.0);
        observer.on_should_ban(&overlay, -101.0, "test");
    }

    #[test]
    fn test_counting_observer() {
        let observer = CountingObserver::new();
        let overlay = test_overlay();
        let event = SwarmScoringEvent::ConnectionSuccess { latency: None };

        observer.on_score_changed(&overlay, 0.0, 1.0, &event);
        observer.on_score_changed(&overlay, 1.0, 2.0, &event);
        observer.on_score_warning(&overlay, -60.0);
        observer.on_should_ban(&overlay, -101.0, "misbehaving");

        assert_eq!(observer.changes.load(Ordering::Relaxed), 2);
        assert_eq!(observer.warnings.load(Ordering::Relaxed), 1);
        assert_eq!(observer.bans.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_arc_observer() {
        let observer: Arc<dyn ScoreObserver> = Arc::new(CountingObserver::new());
        let overlay = test_overlay();
        let event = SwarmScoringEvent::ConnectionTimeout;

        // Should work through Arc
        observer.on_score_changed(&overlay, 0.0, -1.5, &event);
    }
}
