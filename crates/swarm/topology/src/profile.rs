//! Pacing bundles for [`ConnectionProfile`]s.
//!
//! A profile names a build-out posture; this module owns the numbers each
//! posture maps to. The bundle is resolved exactly once, in
//! [`crate::TopologyBehaviourBuilder::try_build`], and only sets values on
//! existing pacing knobs: the evaluation cadence, the GCRA dial-rate quota,
//! the dial-concurrency cap, the bootstrap fill level, and the per-evaluation
//! candidate budgets. No topology logic branches on the profile variant.
//!
//! # How the knobs interact
//!
//! The evaluator refreshes the candidate supply every
//! [`PacingProfile::evaluation_interval`], selecting at most
//! `max_neighbor_candidates + max_balanced_candidates` peers per round. The
//! GCRA bucket ([`PacingProfile::dial_quota`]) then shapes how fast that
//! supply drains into actual dials: a burst up to the bucket size goes out
//! immediately (typically right after a gossip influx), and anything beyond
//! it waits for token replenishment rather than for the next evaluation tick.
//!
//! # Profile numbers
//!
//! - **Balanced** reproduces the long-standing defaults: a 5 s evaluation
//!   interval with a 16 + 16 candidate budget, and a bucket sized to that
//!   budget (32 dials per 5 s, sustained 6.4 dials/s). One full round of
//!   candidates can always be dialed as a burst, so steady-state behaviour is
//!   unchanged from the pre-profile code.
//! - **Aggressive** halves-and-more the cadence (2 s) and raises the budget
//!   to 24 + 24 so a fresh node converts gossip influx into table coverage
//!   quickly; the bucket allows the same burst of 32 but replenishes at 12.8
//!   dials/s, keeping the worst-case sustained rate roughly double Balanced
//!   instead of unbounded.
//! - **Conservative** stretches the cadence to 10 s with an 8 + 8 budget and
//!   a bucket of 8 per 10 s (0.8 dials/s sustained), for metered or
//!   battery-constrained environments where convergence time is traded for
//!   quiet.
//!
//! Bootstrap fill (`bootstrap_target`) scales the same way (24 / 18 / 12):
//! it is the per-bin level a blind node (depth 0) fills toward, floored at
//! the spec saturation threshold by `DepthAwareLimits` so no profile can
//! deadlock the depth climb.

use std::num::NonZeroU32;
use std::time::Duration;

use vertex_net_ratelimiter::Quota;
use vertex_swarm_primitives::ConnectionProfile;

use crate::kademlia::{
    DEFAULT_BOOTSTRAP_TARGET, DEFAULT_MAX_BALANCED_CANDIDATES, DEFAULT_MAX_NEIGHBOR_CANDIDATES,
};

/// Default cadence of connection-evaluation rounds (Balanced).
///
/// Each round reconsiders which bins are under target and refreshes the dial
/// candidate supply. Shorter wastes work on a stable table; longer slows
/// convergence after churn.
const BALANCED_EVALUATION_INTERVAL: Duration = Duration::from_secs(5);

/// Default cap on concurrent in-flight dials (Balanced). Generous because the
/// per-bin routing targets, not this cap, are the real gate on how many dials
/// become connections.
const BALANCED_DIAL_CONCURRENCY: usize = 256;

/// Numeric pacing bundle a [`ConnectionProfile`] resolves to.
///
/// Produced by `PacingProfile::from(profile)`; consumed by
/// [`crate::TopologyBehaviourBuilder::try_build`], which threads each number
/// into the existing config knob it parameterizes. See the module docs for
/// the per-profile values and their rationale.
#[derive(Debug, Clone, Copy)]
pub struct PacingProfile {
    /// Cadence of connection-evaluation rounds: both the behaviour's periodic
    /// trigger and the background evaluator's own fallback tick.
    pub evaluation_interval: Duration,
    /// GCRA quota shaping the discovery dial rate. The bucket size is the
    /// burst absorbed immediately after a candidate influx; the replenish
    /// window sets the sustained rate.
    pub dial_quota: Quota,
    /// Maximum concurrent in-flight dials tracked by the dialer.
    pub dial_concurrency: usize,
    /// Per-bin fill target while no neighborhood is established (depth 0).
    /// Floored at the spec saturation threshold by the depth-aware limits.
    pub bootstrap_target: usize,
    /// Maximum neighborhood (depth-bin) candidates selected per evaluation.
    pub max_neighbor_candidates: usize,
    /// Maximum balanced (non-depth-bin) candidates selected per evaluation.
    pub max_balanced_candidates: usize,
}

/// Const non-zero constructor for quota burst sizes; a zero literal fails at
/// compile time.
const fn burst(n: u32) -> NonZeroU32 {
    match NonZeroU32::new(n) {
        Some(n) => n,
        None => panic!("quota burst must be non-zero"),
    }
}

/// Burst capacity shared by the Aggressive and Balanced buckets: one full
/// Balanced evaluation round (16 + 16 candidates) can always go out at once.
const FULL_ROUND_BURST: NonZeroU32 = burst(32);

impl From<ConnectionProfile> for PacingProfile {
    fn from(profile: ConnectionProfile) -> Self {
        match profile {
            ConnectionProfile::Aggressive => Self {
                evaluation_interval: Duration::from_secs(2),
                // Burst 32, sustained 12.8 dials/s.
                dial_quota: Quota::n_every(FULL_ROUND_BURST, Duration::from_millis(2500)),
                dial_concurrency: BALANCED_DIAL_CONCURRENCY,
                bootstrap_target: 24,
                max_neighbor_candidates: 24,
                max_balanced_candidates: 24,
            },
            ConnectionProfile::Balanced => Self {
                evaluation_interval: BALANCED_EVALUATION_INTERVAL,
                // Burst 32, sustained 6.4 dials/s: exactly one full candidate
                // round per evaluation interval, matching pre-profile pacing.
                dial_quota: Quota::n_every(FULL_ROUND_BURST, BALANCED_EVALUATION_INTERVAL),
                dial_concurrency: BALANCED_DIAL_CONCURRENCY,
                bootstrap_target: DEFAULT_BOOTSTRAP_TARGET,
                max_neighbor_candidates: DEFAULT_MAX_NEIGHBOR_CANDIDATES,
                max_balanced_candidates: DEFAULT_MAX_BALANCED_CANDIDATES,
            },
            ConnectionProfile::Conservative => Self {
                evaluation_interval: Duration::from_secs(10),
                // Burst 8, sustained 0.8 dials/s.
                dial_quota: Quota::n_every(burst(8), Duration::from_secs(10)),
                dial_concurrency: 64,
                bootstrap_target: 12,
                max_neighbor_candidates: 8,
                max_balanced_candidates: 8,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kademlia::KademliaConfig;

    #[test]
    fn balanced_matches_preprofile_defaults() {
        // Balanced is the compatibility profile: its numbers must equal the
        // config defaults so a node without a profile behaves as before.
        let pacing = PacingProfile::from(ConnectionProfile::Balanced);
        let defaults = KademliaConfig::default();

        assert_eq!(pacing.evaluation_interval, Duration::from_secs(5));
        assert_eq!(
            pacing.max_neighbor_candidates,
            defaults.max_neighbor_candidates
        );
        assert_eq!(
            pacing.max_balanced_candidates,
            defaults.max_balanced_candidates
        );
        assert_eq!(pacing.bootstrap_target, DEFAULT_BOOTSTRAP_TARGET);
        assert_eq!(pacing.dial_concurrency, 256);
    }

    #[test]
    fn profiles_order_by_aggressiveness() {
        let aggressive = PacingProfile::from(ConnectionProfile::Aggressive);
        let balanced = PacingProfile::from(ConnectionProfile::Balanced);
        let conservative = PacingProfile::from(ConnectionProfile::Conservative);

        assert!(aggressive.evaluation_interval < balanced.evaluation_interval);
        assert!(balanced.evaluation_interval < conservative.evaluation_interval);

        assert!(aggressive.bootstrap_target > balanced.bootstrap_target);
        assert!(balanced.bootstrap_target > conservative.bootstrap_target);

        assert!(aggressive.max_neighbor_candidates > balanced.max_neighbor_candidates);
        assert!(balanced.max_neighbor_candidates > conservative.max_neighbor_candidates);

        assert!(aggressive.dial_concurrency >= balanced.dial_concurrency);
        assert!(balanced.dial_concurrency > conservative.dial_concurrency);
    }
}
