//! Swarm peer score wrapper with policy, callbacks, and observer support.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use vertex_net_peer_score::PeerScore;
use vertex_swarm_primitives::OverlayAddress;

use crate::config::{SwarmScoringConfig, SwarmScoringEvent};

/// Closure-based callbacks invoked on peer score changes.
pub struct ScoreCallbacks {
    pub on_score_changed:
        Box<dyn Fn(&OverlayAddress, f64, f64, &SwarmScoringEvent) + Send + Sync>,
    pub on_score_warning: Box<dyn Fn(&OverlayAddress, f64) + Send + Sync>,
    pub on_should_ban: Box<dyn Fn(&OverlayAddress, f64, &str) + Send + Sync>,
    pub on_severe_event: Box<dyn Fn(&OverlayAddress, &SwarmScoringEvent) + Send + Sync>,
}

impl ScoreCallbacks {
    /// Create no-op callbacks (all closures do nothing).
    pub fn noop() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

impl Default for ScoreCallbacks {
    fn default() -> Self {
        Self {
            on_score_changed: Box::new(|_, _, _, _| {}),
            on_score_warning: Box::new(|_, _| {}),
            on_should_ban: Box::new(|_, _, _| {}),
            on_severe_event: Box::new(|_, _| {}),
        }
    }
}

/// Swarm-specific peer score with configurable policy and observer support.
pub struct SwarmPeerScore {
    /// Identifies which peer triggered callbacks without threading overlay through every call.
    overlay: OverlayAddress,
    score: Arc<PeerScore>,
    config: Arc<SwarmScoringConfig>,
    callbacks: Arc<ScoreCallbacks>,
    warned: AtomicBool,
}

impl SwarmPeerScore {
    pub fn new(
        overlay: OverlayAddress,
        score: PeerScore,
        config: Arc<SwarmScoringConfig>,
        callbacks: Arc<ScoreCallbacks>,
    ) -> Self {
        Self {
            overlay,
            score: Arc::new(score),
            config,
            callbacks,
            warned: AtomicBool::new(false),
        }
    }

    pub fn with_defaults(overlay: OverlayAddress) -> Self {
        Self::new(
            overlay,
            PeerScore::new(),
            Arc::new(SwarmScoringConfig::default()),
            ScoreCallbacks::noop(),
        )
    }

    #[must_use]
    pub fn score(&self) -> f64 {
        self.score.score()
    }

    pub fn config(&self) -> &SwarmScoringConfig {
        &self.config
    }

    /// Apply configured weight, update latency tracking, and notify observer.
    pub fn record_event(&self, event: SwarmScoringEvent) {
        let old_score = self.score.score();
        let weight = self.config.weight_for(&event);

        // Record latency if present
        if let Some(latency) = event.latency() {
            self.score.record_latency(latency.as_nanos().min(u64::MAX as u128) as u64);
        }

        // Apply weight
        self.score.add_score(weight);

        let new_score = self.score.score();

        // Notify observer
        (self.callbacks.on_score_changed)(&self.overlay, old_score, new_score, &event);

        // Check for severe events
        if event.is_severe() {
            (self.callbacks.on_severe_event)(&self.overlay, &event);
        }

        // Check thresholds
        self.check_thresholds(new_score, &event);
    }

    /// Record latency without affecting score.
    pub fn set_latency(&self, rtt: Duration) {
        self.score.record_latency(rtt.as_nanos() as u64);
    }

    #[must_use]
    pub fn avg_latency(&self) -> Option<Duration> {
        self.score.avg_latency()
    }

    #[must_use]
    pub fn should_ban(&self) -> bool {
        self.config.should_ban(self.score.score())
    }

    #[must_use]
    pub fn snapshot(&self) -> PeerScore {
        (*self.score).clone()
    }

    fn check_thresholds(&self, score: f64, event: &SwarmScoringEvent) {
        if self.config.should_ban(score) {
            let reason = format!("score {:+.1} below threshold after {:?}", score, event);
            (self.callbacks.on_should_ban)(&self.overlay, score, &reason);
            return;
        }

        if self.config.should_warn(score) {
            // Only warn once (CAS: false → true)
            if self.warned.compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
                (self.callbacks.on_score_warning)(&self.overlay, score);
            }
        } else {
            // Reset warning flag if score recovered
            self.warned.store(false, Ordering::Release);
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

    fn counting_callbacks() -> (Arc<ScoreCallbacks>, Arc<AtomicU32>, Arc<AtomicU32>, Arc<AtomicU32>, Arc<AtomicU32>) {
        let changes = Arc::new(AtomicU32::new(0));
        let warnings = Arc::new(AtomicU32::new(0));
        let bans = Arc::new(AtomicU32::new(0));
        let severe = Arc::new(AtomicU32::new(0));

        let cb = Arc::new(ScoreCallbacks {
            on_score_changed: {
                let c = Arc::clone(&changes);
                Box::new(move |_, _, _, _| { c.fetch_add(1, Ordering::Relaxed); })
            },
            on_score_warning: {
                let w = Arc::clone(&warnings);
                Box::new(move |_, _| { w.fetch_add(1, Ordering::Relaxed); })
            },
            on_should_ban: {
                let b = Arc::clone(&bans);
                Box::new(move |_, _, _| { b.fetch_add(1, Ordering::Relaxed); })
            },
            on_severe_event: {
                let s = Arc::clone(&severe);
                Box::new(move |_, _| { s.fetch_add(1, Ordering::Relaxed); })
            },
        });

        (cb, changes, warnings, bans, severe)
    }

    #[test]
    fn test_noop_callbacks() {
        let cb = ScoreCallbacks::noop();
        let overlay = test_overlay(1);
        let event = SwarmScoringEvent::ConnectionSuccess {
            latency: Some(Duration::from_millis(50)),
        };

        // Should not panic
        (cb.on_score_changed)(&overlay, 0.0, 1.0, &event);
        (cb.on_score_warning)(&overlay, -60.0);
        (cb.on_should_ban)(&overlay, -101.0, "test");
        (cb.on_severe_event)(&overlay, &event);
    }

    #[test]
    fn test_counting_callbacks() {
        let changes = Arc::new(AtomicU32::new(0));
        let warnings = Arc::new(AtomicU32::new(0));
        let bans = Arc::new(AtomicU32::new(0));

        let cb = ScoreCallbacks {
            on_score_changed: {
                let c = Arc::clone(&changes);
                Box::new(move |_, _, _, _| { c.fetch_add(1, Ordering::Relaxed); })
            },
            on_score_warning: {
                let w = Arc::clone(&warnings);
                Box::new(move |_, _| { w.fetch_add(1, Ordering::Relaxed); })
            },
            on_should_ban: {
                let b = Arc::clone(&bans);
                Box::new(move |_, _, _| { b.fetch_add(1, Ordering::Relaxed); })
            },
            on_severe_event: Box::new(|_, _| {}),
        };

        let overlay = test_overlay(1);
        let event = SwarmScoringEvent::ConnectionSuccess { latency: None };

        (cb.on_score_changed)(&overlay, 0.0, 1.0, &event);
        (cb.on_score_changed)(&overlay, 1.0, 2.0, &event);
        (cb.on_score_warning)(&overlay, -60.0);
        (cb.on_should_ban)(&overlay, -101.0, "misbehaving");

        assert_eq!(changes.load(Ordering::Relaxed), 2);
        assert_eq!(warnings.load(Ordering::Relaxed), 1);
        assert_eq!(bans.load(Ordering::Relaxed), 1);
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
    fn test_callback_notifications() {
        let (cb, changes, _, _, _) = counting_callbacks();
        let config = Arc::new(SwarmScoringConfig::default());
        let score = SwarmPeerScore::new(test_overlay(1), PeerScore::new(), config, cb);

        score.record_connection_success(None);
        score.record_connection_timeout();

        assert_eq!(changes.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn test_severe_event_notification() {
        let (cb, _, _, _, severe) = counting_callbacks();
        let config = Arc::new(SwarmScoringConfig::default());
        let score = SwarmPeerScore::new(test_overlay(1), PeerScore::new(), config, cb);

        score.record_malicious_behavior();

        assert_eq!(severe.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_warning_notification() {
        let (cb, _, warnings, _, _) = counting_callbacks();
        let config = SwarmScoringConfig::builder().warn_threshold(-10.0).build();
        let score = SwarmPeerScore::new(test_overlay(1), PeerScore::new(), Arc::new(config), cb);

        // Drop below warning threshold
        for _ in 0..10 {
            score.record_connection_timeout();
        }

        // Should only warn once
        assert_eq!(warnings.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_ban_notification() {
        let (cb, _, _, bans, _) = counting_callbacks();
        let config = SwarmScoringConfig::builder().ban_threshold(-20.0).build();
        let score = SwarmPeerScore::new(test_overlay(1), PeerScore::new(), Arc::new(config), cb);

        // Drop below ban threshold
        for _ in 0..15 {
            score.record_connection_timeout();
        }

        assert!(bans.load(Ordering::Relaxed) >= 1);
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
            snapshot,
            Arc::new(SwarmScoringConfig::default()),
            ScoreCallbacks::noop(),
        );

        assert!((score.score() - score2.score()).abs() < 0.01);
    }
}
