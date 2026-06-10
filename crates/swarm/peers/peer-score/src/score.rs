//! Swarm peer score wrapper with policy, callbacks, and observer support.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use vertex_net_peer_score::PeerScore;
use vertex_swarm_primitives::OverlayAddress;

use crate::config::{SwarmScoringConfig, SwarmScoringEvent};

/// Called when a peer's score changes: `(overlay, old_score, new_score, event)`.
pub type ScoreChangedFn = Box<dyn Fn(&OverlayAddress, f64, f64, &SwarmScoringEvent) + Send + Sync>;

/// Called when a peer's score crosses the warning threshold: `(overlay, score)`.
pub type ScoreWarningFn = Box<dyn Fn(&OverlayAddress, f64) + Send + Sync>;

/// Called when a peer should be banned: `(overlay, score, reason)`.
pub type ShouldBanFn = Box<dyn Fn(&OverlayAddress, f64, &str) + Send + Sync>;

/// Called on severe scoring events: `(overlay, event)`.
pub type SevereEventFn = Box<dyn Fn(&OverlayAddress, &SwarmScoringEvent) + Send + Sync>;

/// Action implied by a peer's score after recording an event.
///
/// Returned by [`SwarmPeerScore::record_event`] so the caller can act on
/// threshold crossings through its own report path instead of wiring
/// closures. `Warn` and `Disconnect` are edge-triggered: they are returned
/// when the score crosses the threshold, not on every event below it. `Ban`
/// is level-triggered: it is returned whenever the score is at or below the
/// ban threshold, so severe events that drive the score straight past it
/// surface immediately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreOutcome {
    /// No threshold crossed; no action needed.
    Ok,
    /// Score crossed the warning threshold (emitted once per descent).
    Warn,
    /// Score crossed the disconnect threshold; the peer should be dropped.
    Disconnect,
    /// Score is at or below the ban threshold; the peer should be banned.
    Ban,
}

/// Closure-based callbacks invoked on peer score changes.
///
/// Transitional surface: the successor is a single report path on the peer
/// manager that consumes the [`ScoreOutcome`] returned by
/// [`SwarmPeerScore::record_event`] and maps it to warn, disconnect, or ban
/// actions itself. New consumers should branch on the returned outcome
/// rather than registering callbacks; this type remains only until existing
/// wiring migrates.
pub struct ScoreCallbacks {
    pub on_score_changed: ScoreChangedFn,
    pub on_score_warning: ScoreWarningFn,
    pub on_should_ban: ShouldBanFn,
    pub on_severe_event: SevereEventFn,
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
    ///
    /// Returns the [`ScoreOutcome`] implied by the resulting score so the
    /// caller can warn, disconnect, or ban without registering callbacks.
    /// Threshold logic lives in one place here: warn and disconnect are
    /// edge-triggered on the crossing, ban is level-triggered at or below
    /// the ban threshold.
    pub fn record_event(&self, event: SwarmScoringEvent) -> ScoreOutcome {
        let weight = self.config.weight_for(&event);

        // Record latency if present
        if let Some(latency) = event.latency() {
            self.score
                .record_latency(latency.as_nanos().min(u64::MAX as u128) as u64);
        }

        // Apply weight; old and new come from the same atomic update so
        // threshold crossings are race-free under concurrent recording.
        let (old_score, new_score) = self.score.add_score(weight);

        // Notify observer
        (self.callbacks.on_score_changed)(&self.overlay, old_score, new_score, &event);

        // Check for severe events
        if event.is_severe() {
            (self.callbacks.on_severe_event)(&self.overlay, &event);
        }

        // Check thresholds
        self.check_thresholds(old_score, new_score, &event)
    }

    /// Exponentially decay the score toward zero.
    ///
    /// Delegates to [`PeerScore::decay`]; callers pass elapsed time from
    /// their own heartbeat. Resets the one-shot warning state when the
    /// decayed score has recovered above the warning threshold so a later
    /// descent warns again.
    pub fn decay(&self, half_life_secs: u64, elapsed_secs: u64) {
        self.score.decay(half_life_secs, elapsed_secs);
        if !self.config.should_warn(self.score.score()) {
            self.warned.store(false, Ordering::Release);
        }
    }

    /// Record latency without affecting score.
    pub fn record_latency(&self, rtt: Duration) {
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

    fn check_thresholds(
        &self,
        old_score: f64,
        new_score: f64,
        event: &SwarmScoringEvent,
    ) -> ScoreOutcome {
        if self.config.should_ban(new_score) {
            let reason = format!("score {:+.1} below threshold after {:?}", new_score, event);
            (self.callbacks.on_should_ban)(&self.overlay, new_score, &reason);
            return ScoreOutcome::Ban;
        }

        let mut warned_now = false;
        if self.config.should_warn(new_score) {
            // Only warn once (CAS: false -> true)
            if self
                .warned
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                (self.callbacks.on_score_warning)(&self.overlay, new_score);
                warned_now = true;
            }
        } else {
            // Reset warning flag if score recovered
            self.warned.store(false, Ordering::Release);
        }

        // Edge-triggered on the downward crossing only, so a recovery event
        // climbing out of the ban range never reads as a fresh disconnect.
        // Disconnect outranks warn when one event crosses both thresholds.
        let disconnect_threshold = self.config.disconnect_threshold();
        if old_score > disconnect_threshold && new_score <= disconnect_threshold {
            return ScoreOutcome::Disconnect;
        }
        if warned_now {
            return ScoreOutcome::Warn;
        }
        ScoreOutcome::Ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn test_overlay(n: u8) -> OverlayAddress {
        OverlayAddress::from([n; 32])
    }

    #[allow(clippy::type_complexity)]
    fn counting_callbacks() -> (
        Arc<ScoreCallbacks>,
        Arc<AtomicU32>,
        Arc<AtomicU32>,
        Arc<AtomicU32>,
        Arc<AtomicU32>,
    ) {
        let changes = Arc::new(AtomicU32::new(0));
        let warnings = Arc::new(AtomicU32::new(0));
        let bans = Arc::new(AtomicU32::new(0));
        let severe = Arc::new(AtomicU32::new(0));

        let cb = Arc::new(ScoreCallbacks {
            on_score_changed: {
                let c = Arc::clone(&changes);
                Box::new(move |_, _, _, _| {
                    c.fetch_add(1, Ordering::Relaxed);
                })
            },
            on_score_warning: {
                let w = Arc::clone(&warnings);
                Box::new(move |_, _| {
                    w.fetch_add(1, Ordering::Relaxed);
                })
            },
            on_should_ban: {
                let b = Arc::clone(&bans);
                Box::new(move |_, _, _| {
                    b.fetch_add(1, Ordering::Relaxed);
                })
            },
            on_severe_event: {
                let s = Arc::clone(&severe);
                Box::new(move |_, _| {
                    s.fetch_add(1, Ordering::Relaxed);
                })
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
                Box::new(move |_, _, _, _| {
                    c.fetch_add(1, Ordering::Relaxed);
                })
            },
            on_score_warning: {
                let w = Arc::clone(&warnings);
                Box::new(move |_, _| {
                    w.fetch_add(1, Ordering::Relaxed);
                })
            },
            on_should_ban: {
                let b = Arc::clone(&bans);
                Box::new(move |_, _, _| {
                    b.fetch_add(1, Ordering::Relaxed);
                })
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
    fn test_outcome_warn_is_one_shot_until_recovery() {
        let config = SwarmScoringConfig::builder().warn_threshold(-10.0).build();
        let score = SwarmPeerScore::new(
            test_overlay(1),
            PeerScore::new(),
            Arc::new(config),
            ScoreCallbacks::noop(),
        );

        // Six timeouts (-1.5 each) stay above -10: all Ok.
        for _ in 0..6 {
            assert_eq!(
                score.record_event(SwarmScoringEvent::ConnectionTimeout),
                ScoreOutcome::Ok
            );
        }

        // Seventh crosses -10: Warn exactly once.
        assert_eq!(
            score.record_event(SwarmScoringEvent::ConnectionTimeout),
            ScoreOutcome::Warn
        );
        assert_eq!(
            score.record_event(SwarmScoringEvent::ConnectionTimeout),
            ScoreOutcome::Ok
        );

        // Recover above the warn threshold, then descend again: warns again.
        for _ in 0..3 {
            score.record_event(SwarmScoringEvent::ConnectionSuccess { latency: None });
        }
        assert_eq!(
            score.record_event(SwarmScoringEvent::ConnectionTimeout),
            ScoreOutcome::Warn
        );
    }

    #[test]
    fn test_outcome_disconnect_is_edge_triggered() {
        let score = SwarmPeerScore::with_defaults(test_overlay(1));

        // 0 -> -50: at the warn threshold but not below it.
        assert_eq!(
            score.record_event(SwarmScoringEvent::MaliciousBehavior),
            ScoreOutcome::Ok
        );
        // -50 -> -70: warn crossing.
        assert_eq!(
            score.record_event(SwarmScoringEvent::AccountingViolation),
            ScoreOutcome::Warn
        );
        // -70 -> -72: still above disconnect.
        assert_eq!(
            score.record_event(SwarmScoringEvent::RetrievalFailure),
            ScoreOutcome::Ok
        );
        // -72 -> -82: crosses -75 once.
        assert_eq!(
            score.record_event(SwarmScoringEvent::InvalidData),
            ScoreOutcome::Disconnect
        );
        // -82 -> -84: already below; edge-triggered means Ok.
        assert_eq!(
            score.record_event(SwarmScoringEvent::RetrievalFailure),
            ScoreOutcome::Ok
        );
    }

    #[test]
    fn test_outcome_ban_is_level_triggered() {
        let score = SwarmPeerScore::with_defaults(test_overlay(1));

        score.record_event(SwarmScoringEvent::InvalidData); // -10
        for _ in 0..4 {
            score.record_event(SwarmScoringEvent::AccountingViolation); // -90
        }
        // -90 -> -100 (clamped at the scale minimum): Ban.
        assert_eq!(
            score.record_event(SwarmScoringEvent::AccountingViolation),
            ScoreOutcome::Ban
        );
        // Still at the ban threshold: Ban on every event, not just the edge.
        assert_eq!(
            score.record_event(SwarmScoringEvent::ConnectionTimeout),
            ScoreOutcome::Ban
        );
        assert!(score.should_ban());
    }

    #[test]
    fn test_outcome_recovery_from_ban_range_is_not_disconnect() {
        let score = SwarmPeerScore::with_defaults(test_overlay(1));

        // Drive the score to the ban threshold.
        score.record_event(SwarmScoringEvent::MaliciousBehavior);
        assert_eq!(
            score.record_event(SwarmScoringEvent::MaliciousBehavior),
            ScoreOutcome::Ban
        );

        // A positive event climbing out of the ban range must not read as a
        // fresh downward disconnect crossing. The peer was never warned (the
        // descent jumped straight past the warn band into the ban branch),
        // so the one-shot warning fires here instead.
        assert_eq!(
            score.record_event(SwarmScoringEvent::ConnectionSuccess { latency: None }),
            ScoreOutcome::Warn
        );
        // Subsequent events inside the warn band are quiet.
        assert_eq!(
            score.record_event(SwarmScoringEvent::ConnectionSuccess { latency: None }),
            ScoreOutcome::Ok
        );
    }

    #[test]
    fn test_decay_resets_warning_state() {
        let score = SwarmPeerScore::with_defaults(test_overlay(1));

        // Cross the warn threshold (-50) once.
        score.record_event(SwarmScoringEvent::MaliciousBehavior); // -50
        assert_eq!(
            score.record_event(SwarmScoringEvent::ProtocolError), // -53
            ScoreOutcome::Warn
        );

        // Decay well above the warn threshold, then descend again: a fresh
        // warning fires because decay reset the one-shot state.
        score.decay(60, 600); // -53 * 2^-10, near zero
        assert!(score.score() > -1.0);
        assert_eq!(
            score.record_event(SwarmScoringEvent::AccountingViolation), // about -20
            ScoreOutcome::Ok
        );
        assert_eq!(
            score.record_event(SwarmScoringEvent::AccountingViolation), // about -40
            ScoreOutcome::Ok
        );
        assert_eq!(
            score.record_event(SwarmScoringEvent::AccountingViolation), // about -60
            ScoreOutcome::Warn
        );
    }

    #[test]
    fn test_outcome_severe_fast_path() {
        let (cb, _, _, bans, severe) = counting_callbacks();
        let config = Arc::new(SwarmScoringConfig::default());
        let score = SwarmPeerScore::new(test_overlay(1), PeerScore::new(), config, cb);

        // Two severe events drive the score straight to the ban threshold.
        assert_eq!(
            score.record_event(SwarmScoringEvent::MaliciousBehavior),
            ScoreOutcome::Ok
        );
        assert_eq!(
            score.record_event(SwarmScoringEvent::MaliciousBehavior),
            ScoreOutcome::Ban
        );
        assert_eq!(severe.load(Ordering::Relaxed), 2);
        assert_eq!(bans.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_outcome_disconnect_outranks_warn_on_double_crossing() {
        let (cb, _, warnings, _, _) = counting_callbacks();
        let config = Arc::new(SwarmScoringConfig::default());
        let score = SwarmPeerScore::new(test_overlay(1), PeerScore::new(), config, cb);

        // 0 -> -30: above both thresholds.
        score.record_event(SwarmScoringEvent::AccountingViolation);
        score.record_event(SwarmScoringEvent::InvalidData);
        // -30 -> -80 crosses warn (-50) and disconnect (-75) in one event:
        // the outcome is Disconnect, but the warn callback shim still fires.
        assert_eq!(
            score.record_event(SwarmScoringEvent::MaliciousBehavior),
            ScoreOutcome::Disconnect
        );
        assert_eq!(warnings.load(Ordering::Relaxed), 1);
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
