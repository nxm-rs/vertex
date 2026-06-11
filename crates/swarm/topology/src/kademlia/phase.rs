//! Explicit topology phase machine.
//!
//! Names the connection-building lifecycle that was previously implicit in
//! the depth-aware limits (depth 0 selecting the bootstrap fill targets).
//! The phase is derived deterministically from observable routing state, so
//! it can always be re-derived from a fresh snapshot rather than depending
//! on edge-triggered bookkeeping:
//!
//! - [`TopologyPhase::Bootstrap`]: published depth is 0. The known table is
//!   too thin to anchor a neighborhood, and every bin fills toward the
//!   bootstrap target.
//! - [`TopologyPhase::Converging`]: depth is above 0 but moved within the
//!   stability window, or the neighborhood has not yet reached the
//!   saturation threshold. Connection building is still raising the
//!   unsaturated frontier.
//! - [`TopologyPhase::Stable`]: depth is above 0, has not moved for the
//!   stability window, and the neighborhood holds at least the saturation
//!   threshold in connected peers. Maintenance pacing suffices.
//!
//! Transitions are computed by [`PhaseTracker::evaluate`], which the routing
//! layer drives from the depth-publication points (peer connect and
//! disconnect) and the periodic connection evaluator. A `Stable` node falls
//! back to `Converging` when depth moves or neighborhood saturation is
//! lost, and all the way to `Bootstrap` only when depth returns to 0.

use std::time::Duration;

use web_time::Instant;

use vertex_swarm_primitives::NeighborhoodDepth;

/// Phase of the topology connection-building lifecycle.
///
/// Derived from published depth, depth movement within the stability
/// window, and neighborhood saturation; see the module docs for the exact
/// rules. Serialized in `snake_case` for metric labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum TopologyPhase {
    /// Published depth is 0: fill every bin toward the bootstrap target.
    Bootstrap,
    /// Depth is climbing or the neighborhood is unsaturated: prioritize
    /// dials that raise the unsaturated frontier.
    Converging,
    /// Depth steady for the stability window with a saturated
    /// neighborhood: maintenance pacing, inbound mostly suffices.
    Stable,
}

impl TopologyPhase {
    /// All phases, in lifecycle order. Used to publish the per-phase gauge.
    pub const ALL: [Self; 3] = [Self::Bootstrap, Self::Converging, Self::Stable];
}

/// A committed phase transition, returned by [`PhaseTracker::evaluate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PhaseTransition {
    /// Phase before the transition.
    pub(crate) from: TopologyPhase,
    /// Phase after the transition.
    pub(crate) to: TopologyPhase,
    /// Neighborhood depth observed at the transition.
    pub(crate) depth: NeighborhoodDepth,
    /// Time spent in the previous phase.
    pub(crate) time_in_phase: Duration,
}

/// Owns the current phase and computes transitions from observed state.
///
/// Pure state machine: callers supply the observation time, so tests drive
/// it with simulated clocks. Recording (log lines, metrics, events) is the
/// caller's concern.
pub(crate) struct PhaseTracker {
    phase: TopologyPhase,
    entered_at: Instant,
    stability_window: Duration,
    last_depth: NeighborhoodDepth,
    last_depth_change: Instant,
}

impl PhaseTracker {
    /// Start tracking in [`TopologyPhase::Bootstrap`] at `now`.
    pub(crate) fn new(stability_window: Duration, now: Instant) -> Self {
        Self {
            phase: TopologyPhase::Bootstrap,
            entered_at: now,
            stability_window,
            last_depth: NeighborhoodDepth::ZERO,
            last_depth_change: now,
        }
    }

    /// The current phase.
    pub(crate) fn phase(&self) -> TopologyPhase {
        self.phase
    }

    /// Time spent in the current phase as of `now`.
    pub(crate) fn time_in_phase(&self, now: Instant) -> Duration {
        now.duration_since(self.entered_at)
    }

    /// Re-derive the phase from the observed state and commit a transition
    /// when it moved.
    ///
    /// `depth` is the published neighborhood depth and
    /// `neighborhood_saturated` whether the bins inside the depth boundary
    /// together hold at least the saturation threshold in connected peers
    /// (the same condition as `ReadinessSnapshot::is_saturated`). Depth
    /// movement is timestamped here, so churn that leaves the derived phase
    /// unchanged commits nothing and produces no transition spam.
    pub(crate) fn evaluate(
        &mut self,
        depth: NeighborhoodDepth,
        neighborhood_saturated: bool,
        now: Instant,
    ) -> Option<PhaseTransition> {
        if depth != self.last_depth {
            self.last_depth = depth;
            self.last_depth_change = now;
        }

        let next = if depth == NeighborhoodDepth::ZERO {
            TopologyPhase::Bootstrap
        } else if !neighborhood_saturated
            || now.duration_since(self.last_depth_change) < self.stability_window
        {
            TopologyPhase::Converging
        } else {
            TopologyPhase::Stable
        };

        if next == self.phase {
            return None;
        }

        let transition = PhaseTransition {
            from: self.phase,
            to: next,
            depth,
            time_in_phase: self.time_in_phase(now),
        };
        self.phase = next;
        self.entered_at = now;
        Some(transition)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_primitives::Bin;

    const WINDOW: Duration = Duration::from_secs(60);

    fn d(n: u8) -> NeighborhoodDepth {
        NeighborhoodDepth::new(Bin::new(n).expect("valid bin"))
    }

    fn tracker(now: Instant) -> PhaseTracker {
        PhaseTracker::new(WINDOW, now)
    }

    #[test]
    fn starts_in_bootstrap() {
        let now = Instant::now();
        let t = tracker(now);
        assert_eq!(t.phase(), TopologyPhase::Bootstrap);
        assert_eq!(t.time_in_phase(now), Duration::ZERO);
    }

    #[test]
    fn stays_bootstrap_while_depth_zero() {
        let base = Instant::now();
        let mut t = tracker(base);
        assert_eq!(t.evaluate(d(0), false, base + WINDOW), None);
        assert_eq!(t.evaluate(d(0), false, base + WINDOW * 2), None);
        assert_eq!(t.phase(), TopologyPhase::Bootstrap);
    }

    #[test]
    fn bootstrap_to_converging_on_first_depth_climb() {
        let base = Instant::now();
        let mut t = tracker(base);

        let transition = t
            .evaluate(d(2), false, base + Duration::from_secs(5))
            .expect("depth climb must transition");
        assert_eq!(transition.from, TopologyPhase::Bootstrap);
        assert_eq!(transition.to, TopologyPhase::Converging);
        assert_eq!(transition.depth, d(2));
        assert_eq!(transition.time_in_phase, Duration::from_secs(5));
        assert_eq!(t.phase(), TopologyPhase::Converging);
    }

    #[test]
    fn converging_to_stable_after_window_with_saturation() {
        let base = Instant::now();
        let mut t = tracker(base);
        t.evaluate(d(2), true, base).expect("to converging");

        // Saturated but depth moved more recently than the window: hold.
        assert_eq!(t.evaluate(d(2), true, base + WINDOW / 2), None);
        assert_eq!(t.phase(), TopologyPhase::Converging);

        let transition = t
            .evaluate(d(2), true, base + WINDOW)
            .expect("window elapsed with saturation");
        assert_eq!(transition.from, TopologyPhase::Converging);
        assert_eq!(transition.to, TopologyPhase::Stable);
        assert_eq!(transition.time_in_phase, WINDOW);
    }

    #[test]
    fn converging_holds_without_saturation_even_after_window() {
        let base = Instant::now();
        let mut t = tracker(base);
        t.evaluate(d(2), false, base).expect("to converging");

        assert_eq!(t.evaluate(d(2), false, base + WINDOW * 2), None);
        assert_eq!(t.phase(), TopologyPhase::Converging);

        // Saturation arriving later (depth long steady) completes the climb.
        let transition = t
            .evaluate(d(2), true, base + WINDOW * 3)
            .expect("saturation reached");
        assert_eq!(transition.to, TopologyPhase::Stable);
    }

    #[test]
    fn stable_falls_back_to_converging_on_depth_change() {
        let base = Instant::now();
        let mut t = tracker(base);
        t.evaluate(d(2), true, base).expect("to converging");
        t.evaluate(d(2), true, base + WINDOW).expect("to stable");

        let transition = t
            .evaluate(d(3), true, base + WINDOW * 2)
            .expect("depth moved");
        assert_eq!(transition.from, TopologyPhase::Stable);
        assert_eq!(transition.to, TopologyPhase::Converging);
    }

    #[test]
    fn stable_falls_back_to_converging_on_saturation_loss() {
        let base = Instant::now();
        let mut t = tracker(base);
        t.evaluate(d(2), true, base).expect("to converging");
        t.evaluate(d(2), true, base + WINDOW).expect("to stable");

        let transition = t
            .evaluate(d(2), false, base + WINDOW + Duration::from_secs(1))
            .expect("saturation lost");
        assert_eq!(transition.from, TopologyPhase::Stable);
        assert_eq!(transition.to, TopologyPhase::Converging);
    }

    #[test]
    fn collapse_to_depth_zero_returns_to_bootstrap() {
        let base = Instant::now();
        let mut t = tracker(base);
        t.evaluate(d(2), true, base).expect("to converging");
        t.evaluate(d(2), true, base + WINDOW).expect("to stable");

        let transition = t
            .evaluate(d(0), false, base + WINDOW * 2)
            .expect("depth collapsed");
        assert_eq!(transition.from, TopologyPhase::Stable);
        assert_eq!(transition.to, TopologyPhase::Bootstrap);
    }

    #[test]
    fn churn_within_window_produces_no_transition_spam() {
        let base = Instant::now();
        let mut t = tracker(base);
        t.evaluate(d(2), true, base).expect("to converging");

        // Depth flaps every few seconds; the derived phase never leaves
        // Converging, so no transitions are committed.
        for i in 1..20u64 {
            let depth = if i % 2 == 0 { d(2) } else { d(3) };
            assert_eq!(
                t.evaluate(depth, true, base + Duration::from_secs(i * 5)),
                None
            );
        }
        assert_eq!(t.phase(), TopologyPhase::Converging);

        // Only once depth holds still for the full window does it settle.
        let settled = base + Duration::from_secs(19 * 5) + WINDOW;
        let transition = t.evaluate(d(3), true, settled).expect("settled");
        assert_eq!(transition.to, TopologyPhase::Stable);
    }

    #[test]
    fn time_in_phase_resets_on_transition() {
        let base = Instant::now();
        let mut t = tracker(base);
        t.evaluate(d(2), true, base + Duration::from_secs(10))
            .expect("to converging");
        assert_eq!(
            t.time_in_phase(base + Duration::from_secs(25)),
            Duration::from_secs(15)
        );
    }

    #[test]
    fn phase_labels_are_snake_case() {
        let labels: Vec<&'static str> = TopologyPhase::ALL.iter().map(|p| (*p).into()).collect();
        assert_eq!(labels, ["bootstrap", "converging", "stable"]);
    }
}
