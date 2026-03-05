//! Swarm peer score wrapper with policy and observer support.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use vertex_net_peer_score::{PeerScore, PeerScoreSnapshot};
use vertex_swarm_primitives::OverlayAddress;

use crate::callbacks::ScoreObserver;
use crate::config::{SwarmScoringConfig, SwarmScoringEvent};

/// Swarm-specific peer score with configurable policy and observer support.
///
/// Wraps `PeerScore` and adds:
/// - Configurable scoring weights via `SwarmScoringConfig`
/// - Observer callbacks for score changes
/// - Swarm-specific event handling
pub struct SwarmPeerScore {
    /// Stored here so observer callbacks can identify which peer triggered the event,
    /// without threading overlay through every scoring method call.
    overlay: OverlayAddress,
    score: Arc<PeerScore>,
    config: Arc<SwarmScoringConfig>,
    observer: Arc<dyn ScoreObserver>,
    warned: RwLock<bool>,
}

impl SwarmPeerScore {
    /// Create a new score tracker for a peer.
    pub fn new(
        overlay: OverlayAddress,
        score: PeerScore,
        config: Arc<SwarmScoringConfig>,
        observer: Arc<dyn ScoreObserver>,
    ) -> Self {
        Self {
            overlay,
            score: Arc::new(score),
            config,
            observer,
            warned: RwLock::new(false),
        }
    }

    /// Create with default config and no-op observer.
    pub fn with_defaults(overlay: OverlayAddress) -> Self {
        Self::new(
            overlay,
            PeerScore::new(),
            Arc::new(SwarmScoringConfig::default()),
            Arc::new(crate::callbacks::NoOpScoreObserver),
        )
    }

    /// Get the current score.
    #[must_use]
    pub fn score(&self) -> f64 {
        self.score.score()
    }

    /// Get a clone of the underlying PeerScore Arc.
    pub fn inner(&self) -> Arc<PeerScore> {
        Arc::clone(&self.score)
    }

    /// Access the scoring config.
    pub fn config(&self) -> &SwarmScoringConfig {
        &self.config
    }

    /// Record a scoring event.
    ///
    /// Applies the configured weight for the event type, updates latency
    /// tracking if applicable, and notifies the observer.
    pub fn record_event(&self, event: SwarmScoringEvent) {
        let old_score = self.score.score();
        let weight = self.config.weight_for(&event);

        // Record latency if present
        if let Some(latency) = event.latency() {
            self.score.record_latency(latency.as_nanos() as u64);
        }

        // Apply weight
        self.score.add_score(weight);

        let new_score = self.score.score();

        // Notify observer
        self.observer
            .on_score_changed(&self.overlay, old_score, new_score, &event);

        // Check for severe events
        if event.is_severe() {
            self.observer.on_severe_event(&self.overlay, &event);
        }

        // Check thresholds
        self.check_thresholds(new_score, &event);
    }

    /// Set latency without affecting score (for latency-only measurements).
    pub fn set_latency(&self, rtt: Duration) {
        self.score.record_latency(rtt.as_nanos() as u64);
    }

    /// Get average latency if samples exist.
    #[must_use]
    pub fn avg_latency(&self) -> Option<Duration> {
        self.score.avg_latency()
    }

    /// Check if peer should be banned based on current score.
    #[must_use]
    pub fn should_ban(&self) -> bool {
        self.config.should_ban(self.score.score())
    }

    /// Create a snapshot for persistence.
    #[must_use]
    pub fn snapshot(&self) -> PeerScoreSnapshot {
        PeerScoreSnapshot::from(&self.score)
    }

    fn check_thresholds(&self, score: f64, event: &SwarmScoringEvent) {
        // Check ban threshold
        if self.config.should_ban(score) {
            let reason = format!("score {:+.1} below threshold after {:?}", score, event);
            self.observer.on_should_ban(&self.overlay, score, &reason);
            return;
        }

        // Check warning threshold (only warn once)
        // Release lock before calling observer to prevent deadlocks
        let should_warn = if self.config.should_warn(score) {
            let mut warned = self.warned.write();
            if *warned {
                false
            } else {
                *warned = true;
                true
            }
        } else {
            // Reset warning flag if score recovered
            *self.warned.write() = false;
            false
        };

        if should_warn {
            self.observer.on_score_warning(&self.overlay, score);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn test_overlay(n: u8) -> OverlayAddress {
        OverlayAddress::from([n; 32])
    }

    struct TestObserver {
        changes: AtomicU32,
        warnings: AtomicU32,
        bans: AtomicU32,
        severe: AtomicU32,
    }

    impl TestObserver {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                changes: AtomicU32::new(0),
                warnings: AtomicU32::new(0),
                bans: AtomicU32::new(0),
                severe: AtomicU32::new(0),
            })
        }
    }

    impl ScoreObserver for TestObserver {
        fn on_score_changed(
            &self,
            _overlay: &OverlayAddress,
            _old: f64,
            _new: f64,
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

        fn on_severe_event(&self, _overlay: &OverlayAddress, _event: &SwarmScoringEvent) {
            self.severe.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn test_new_score() {
        let score = SwarmPeerScore::with_defaults(test_overlay(1));
        assert_eq!(score.score(), 0.0);
        assert!(!score.should_ban());
    }

    #[test]
    fn test_record_connection_success() {
        let score = SwarmPeerScore::with_defaults(test_overlay(1));
        score.record_connection_success(Some(Duration::from_millis(50)));

        assert!(score.score() > 0.0);
        assert!(score.avg_latency().is_some());
    }

    #[test]
    fn test_record_connection_timeout() {
        let score = SwarmPeerScore::with_defaults(test_overlay(1));
        score.record_connection_timeout();

        assert!(score.score() < 0.0);
    }

    #[test]
    fn test_observer_notifications() {
        let observer = TestObserver::new();
        let config = Arc::new(SwarmScoringConfig::default());
        let score = SwarmPeerScore::new(test_overlay(1), PeerScore::new(), config, Arc::clone(&observer) as _);

        score.record_connection_success(None);
        score.record_connection_timeout();

        assert_eq!(observer.changes.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn test_severe_event_notification() {
        let observer = TestObserver::new();
        let config = Arc::new(SwarmScoringConfig::default());
        let score = SwarmPeerScore::new(test_overlay(1), PeerScore::new(), config, Arc::clone(&observer) as _);

        score.record_malicious_behavior();

        assert_eq!(observer.severe.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_warning_notification() {
        let observer = TestObserver::new();
        let config = SwarmScoringConfig::builder().warn_threshold(-10.0).build();
        let score = SwarmPeerScore::new(test_overlay(1), PeerScore::new(), Arc::new(config), Arc::clone(&observer) as _);

        // Drop below warning threshold
        for _ in 0..10 {
            score.record_connection_timeout();
        }

        // Should only warn once
        assert_eq!(observer.warnings.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_ban_notification() {
        let observer = TestObserver::new();
        let config = SwarmScoringConfig::builder().ban_threshold(-20.0).build();
        let score = SwarmPeerScore::new(test_overlay(1), PeerScore::new(), Arc::new(config), Arc::clone(&observer) as _);

        // Drop below ban threshold
        for _ in 0..15 {
            score.record_connection_timeout();
        }

        assert!(observer.bans.load(Ordering::Relaxed) >= 1);
        assert!(score.should_ban());
    }

    #[test]
    fn test_snapshot_roundtrip() {
        let score = SwarmPeerScore::with_defaults(test_overlay(1));
        score.record_connection_success(Some(Duration::from_millis(100)));
        score.record_connection_success(Some(Duration::from_millis(50)));

        let snapshot = score.snapshot();

        let score2 = SwarmPeerScore::new(
            test_overlay(2),
            PeerScore::from(&snapshot),
            Arc::new(SwarmScoringConfig::default()),
            Arc::new(crate::callbacks::NoOpScoreObserver),
        );

        assert!((score.score() - score2.score()).abs() < 0.01);
    }
}
