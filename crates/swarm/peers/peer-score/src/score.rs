//! Swarm peer score wrapper with threshold policy.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use vertex_net_peer_score::PeerScore;

use crate::config::{SwarmScoringConfig, SwarmScoringEvent};

/// Action implied by a peer's score after recording an event.
///
/// Returned by [`SwarmPeerScore::record_event`] so the caller can act on
/// threshold crossings through its own report path. `Warn` and `Disconnect`
/// are edge-triggered: they are returned when the score crosses the
/// threshold, not on every event below it. `Ban` is level-triggered: it is
/// returned whenever the score is at or below the ban threshold, so severe
/// events that drive the score straight past it surface immediately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
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

/// Result of recording a scoring event: the implied action plus the score
/// transition that produced it.
///
/// `old_score` and `new_score` come from the same atomic update, so callers
/// that maintain score aggregates (for example the peer manager's per-bucket
/// score distribution gauges) can apply the transition without racing
/// concurrent reports for the same peer.
#[derive(Debug, Clone, Copy)]
pub struct ScoreChange {
    /// Action implied by the new score.
    pub outcome: ScoreOutcome,
    /// Score before the event was applied.
    pub old_score: f64,
    /// Score after the event was applied.
    pub new_score: f64,
}

/// Swarm-specific peer score with configurable threshold policy.
pub struct SwarmPeerScore {
    score: Arc<PeerScore>,
    config: Arc<SwarmScoringConfig>,
    warned: AtomicBool,
}

impl SwarmPeerScore {
    pub fn new(score: PeerScore, config: Arc<SwarmScoringConfig>) -> Self {
        Self {
            score: Arc::new(score),
            config,
            warned: AtomicBool::new(false),
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(PeerScore::new(), Arc::new(SwarmScoringConfig::default()))
    }

    #[must_use]
    pub fn score(&self) -> f64 {
        self.score.score()
    }

    pub fn config(&self) -> &SwarmScoringConfig {
        &self.config
    }

    /// Apply the configured weight and update latency tracking.
    ///
    /// Returns the [`ScoreOutcome`] implied by the resulting score so the
    /// caller can warn, disconnect, or ban. Threshold logic lives in one
    /// place here: warn and disconnect are edge-triggered on the crossing,
    /// ban is level-triggered at or below the ban threshold.
    pub fn record_event(&self, event: SwarmScoringEvent) -> ScoreOutcome {
        self.record_event_change(event).outcome
    }

    /// [`Self::record_event`] plus the atomic score transition.
    ///
    /// Use this variant when the caller maintains an aggregate over peer
    /// scores and needs the exact old and new values of this update.
    pub fn record_event_change(&self, event: SwarmScoringEvent) -> ScoreChange {
        let weight = self.config.weight_for(&event);

        // Record latency if present
        if let Some(latency) = event.latency() {
            self.score
                .record_latency(latency.as_nanos().min(u64::MAX as u128) as u64);
        }

        // Apply weight; old and new come from the same atomic update so
        // threshold crossings are race-free under concurrent recording.
        let (old_score, new_score) = self.score.add_score(weight);

        ScoreChange {
            outcome: self.check_thresholds(old_score, new_score),
            old_score,
            new_score,
        }
    }

    /// Exponentially decay the score toward zero, returning `(old, new)`.
    ///
    /// Delegates to [`PeerScore::decay`]; callers pass elapsed time from
    /// their own heartbeat. Resets the one-shot warning state when the
    /// decayed score has recovered above the warning threshold so a later
    /// descent warns again. The returned transition lets callers keep score
    /// aggregates consistent under concurrent recording.
    pub fn decay(&self, half_life_secs: u64, elapsed_secs: u64) -> (f64, f64) {
        let (old_score, new_score) = self.score.decay(half_life_secs, elapsed_secs);
        if !self.config.should_warn(new_score) {
            self.warned.store(false, Ordering::Release);
        }
        (old_score, new_score)
    }

    /// Reset the score to `score` (clamped to the scale), returning
    /// `(old, new)`.
    ///
    /// The one-shot warning state is aligned with the new value: a reset
    /// into the warn band does not fire a fresh warning on the next
    /// negative event (the caller already acted on the peer), while a reset
    /// above the warn threshold re-arms the warning. The peer manager uses
    /// this when a timed ban expires to restart the peer at the disconnect
    /// threshold.
    pub fn reset(&self, score: f64) -> (f64, f64) {
        let (old_score, new_score) = self.score.set_score(score);
        self.warned
            .store(self.config.should_warn(new_score), Ordering::Release);
        (old_score, new_score)
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

    fn check_thresholds(&self, old_score: f64, new_score: f64) -> ScoreOutcome {
        if self.config.should_ban(new_score) {
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

    #[test]
    fn test_new_score() {
        let score = SwarmPeerScore::with_defaults();
        assert_eq!(score.score(), 0.0);
        assert!(!score.should_ban());
    }

    #[test]
    fn test_record_connection_success() {
        let score = SwarmPeerScore::with_defaults();
        score.record_connection_success(Some(Duration::from_millis(50)));

        assert!(score.score() > 0.0);
        assert!(score.avg_latency().is_some());
    }

    #[test]
    fn test_record_connection_timeout() {
        let score = SwarmPeerScore::with_defaults();
        score.record_connection_timeout();

        assert!(score.score() < 0.0);
    }

    #[test]
    fn test_record_event_change_reports_atomic_transition() {
        let score = SwarmPeerScore::with_defaults();

        let change = score.record_event_change(SwarmScoringEvent::ConnectionSuccess {
            latency: Some(Duration::from_millis(50)),
        });
        assert_eq!(change.old_score, 0.0);
        assert!(change.new_score > 0.0);
        assert_eq!(change.outcome, ScoreOutcome::Ok);

        let next = score.record_event_change(SwarmScoringEvent::ConnectionTimeout);
        assert_eq!(next.old_score, change.new_score);
        assert!(next.new_score < next.old_score);
    }

    #[test]
    fn test_outcome_warn_is_one_shot_until_recovery() {
        let config = SwarmScoringConfig::builder().warn_threshold(-10.0).build();
        let score = SwarmPeerScore::new(PeerScore::new(), Arc::new(config));

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
        let score = SwarmPeerScore::with_defaults();

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
        let score = SwarmPeerScore::with_defaults();

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
        let score = SwarmPeerScore::with_defaults();

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
        let score = SwarmPeerScore::with_defaults();

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
    fn test_reset_returns_transition_and_aligns_warning() {
        let score = SwarmPeerScore::with_defaults();

        // Reset into the warn band (-75 is below the -50 warn threshold).
        let (old, new) = score.reset(-75.0);
        assert_eq!(old, 0.0);
        assert_eq!(new, -75.0);
        assert_eq!(score.score(), -75.0);

        // The warning state matches the band: no fresh warning fires on the
        // next negative event.
        assert_eq!(
            score.record_event(SwarmScoringEvent::ProtocolError),
            ScoreOutcome::Ok
        );

        // Decay back above the warn threshold re-arms the warning; a fresh
        // descent warns again.
        score.decay(60, 600);
        assert!(score.score() > -1.0);
        score.record_event(SwarmScoringEvent::AccountingViolation); // about -20
        score.record_event(SwarmScoringEvent::AccountingViolation); // about -40
        assert_eq!(
            score.record_event(SwarmScoringEvent::AccountingViolation), // about -60
            ScoreOutcome::Warn
        );
    }

    #[test]
    fn test_reset_above_warn_rearms_warning() {
        let score = SwarmPeerScore::with_defaults();

        // Warn once on the way down.
        score.record_event(SwarmScoringEvent::MaliciousBehavior); // -50
        assert_eq!(
            score.record_event(SwarmScoringEvent::ProtocolError), // -53
            ScoreOutcome::Warn
        );

        // Reset to neutral: warning is re-armed for the next descent.
        let (old, new) = score.reset(0.0);
        assert_eq!(old, -53.0);
        assert_eq!(new, 0.0);
        score.record_event(SwarmScoringEvent::MaliciousBehavior); // -50
        assert_eq!(
            score.record_event(SwarmScoringEvent::ProtocolError), // -53
            ScoreOutcome::Warn
        );
    }

    #[test]
    fn test_decay_returns_transition() {
        let score = SwarmPeerScore::with_defaults();
        score.reset(-40.0);
        let (old, new) = score.decay(600, 600);
        assert!((old + 40.0).abs() < 0.001);
        assert!((new + 20.0).abs() < 0.001);
    }

    #[test]
    fn test_outcome_severe_fast_path() {
        let score = SwarmPeerScore::with_defaults();

        // Two severe events drive the score straight to the ban threshold.
        assert_eq!(
            score.record_event(SwarmScoringEvent::MaliciousBehavior),
            ScoreOutcome::Ok
        );
        assert_eq!(
            score.record_event(SwarmScoringEvent::MaliciousBehavior),
            ScoreOutcome::Ban
        );
    }

    #[test]
    fn test_outcome_disconnect_outranks_warn_on_double_crossing() {
        let score = SwarmPeerScore::with_defaults();

        // 0 -> -30: above both thresholds.
        score.record_event(SwarmScoringEvent::AccountingViolation);
        score.record_event(SwarmScoringEvent::InvalidData);
        // -30 -> -80 crosses warn (-50) and disconnect (-75) in one event:
        // the outcome is Disconnect, and the one-shot warning state is
        // consumed so the next event in the warn band stays quiet.
        assert_eq!(
            score.record_event(SwarmScoringEvent::MaliciousBehavior),
            ScoreOutcome::Disconnect
        );
        assert_eq!(
            score.record_event(SwarmScoringEvent::ConnectionSuccess { latency: None }),
            ScoreOutcome::Ok
        );
    }

    #[test]
    fn test_outcome_labels_are_snake_case() {
        let label: &'static str = ScoreOutcome::Disconnect.into();
        assert_eq!(label, "disconnect");
        assert_eq!(ScoreOutcome::Ban.to_string(), "ban");
    }

    #[test]
    fn test_snapshot_roundtrip() {
        let score = SwarmPeerScore::with_defaults();
        score.record_connection_success(Some(Duration::from_millis(100)));
        score.record_connection_success(Some(Duration::from_millis(50)));

        let snapshot = score.snapshot();

        let score2 = SwarmPeerScore::new(snapshot, Arc::new(SwarmScoringConfig::default()));

        assert!((score.score() - score2.score()).abs() < 0.01);
    }
}
