//! Kademlia routing configuration.
//!
//! [`KademliaConfig`] bundles the depth-aware per-bin targets (see the `limits`
//! module) with the per-round candidate budget. Each connection-evaluation round
//! selects at most `max_neighbor_candidates + max_balanced_candidates` new peers
//! to dial, split between the neighborhood (depth) bins and the balanced
//! (non-depth) bins below them. The 16 / 16 default gives a 32-candidate budget
//! per round, roughly one slot per Kademlia bin, so a single round can make
//! progress on every bin without flooding the dialer. The depth-aware targets,
//! not this budget, decide how many candidates actually become connections.

use std::time::Duration;

use super::limits::DepthAwareLimits;

/// Max new neighborhood (depth-bin) candidates enqueued per evaluation round.
const DEFAULT_MAX_NEIGHBOR_CANDIDATES: usize = 16;
/// Max new balanced (non-depth-bin) candidates enqueued per evaluation round.
const DEFAULT_MAX_BALANCED_CANDIDATES: usize = 16;
/// Default stability window for the topology phase machine.
const DEFAULT_PHASE_STABILITY_WINDOW: Duration = Duration::from_secs(60);

/// Default window the neighborhood must stay saturated at an unchanged depth
/// before it counts as ready (see `TopologyHandle::wait_until_neighborhood_ready`).
///
/// Long enough to absorb the connect/evict churn of the first dial rounds, short
/// enough that a converged storer can start pull-syncing promptly.
const DEFAULT_NEIGHBORHOOD_STABILITY_WINDOW: Duration = Duration::from_secs(30);

/// Default stability window before a marginal depth lowering is published.
///
/// A recomputed depth below the published depth with a saturation deficit of
/// at most one peer is held back for this long; if the deficit persists for
/// the whole window the lower depth is published, and if the table recovers
/// in the meantime nothing is ever published. Thirty seconds rides out a
/// reconnect cycle of a single churning frontier peer without letting a real
/// (slow) capacity loss go unreported for long.
const DEFAULT_DEPTH_LOWER_WINDOW: Duration = Duration::from_secs(30);

/// Configuration for Kademlia routing.
#[derive(Debug, Clone)]
pub struct KademliaConfig {
    /// Depth-aware per-bin capacity limits.
    pub(crate) limits: DepthAwareLimits,
    /// Maximum concurrent pending candidates for neighbor (depth) bins.
    pub(crate) max_neighbor_candidates: usize,
    /// Maximum concurrent pending candidates for balanced (non-depth) bins.
    pub(crate) max_balanced_candidates: usize,
    /// How long the neighborhood must stay saturated at an unchanged depth
    /// before it is considered stable (the gate pull-syncing waits on).
    pub(crate) neighborhood_stability_window: Duration,
    /// Stability window for lowering the published neighborhood depth when
    /// the saturation deficit is a single peer (see
    /// [`Self::with_depth_lower_window`]).
    pub(crate) depth_lower_window: Duration,
    /// How long the neighborhood depth must hold still (with a saturated
    /// neighborhood) before the topology phase machine reports
    /// [`super::TopologyPhase::Stable`]. Any depth movement inside the
    /// window keeps the node in `Converging`, so churn cannot flap the
    /// phase. Default 60s.
    pub(crate) phase_stability_window: Duration,
}

impl Default for KademliaConfig {
    fn default() -> Self {
        Self {
            limits: DepthAwareLimits::default(),
            max_neighbor_candidates: DEFAULT_MAX_NEIGHBOR_CANDIDATES,
            max_balanced_candidates: DEFAULT_MAX_BALANCED_CANDIDATES,
            neighborhood_stability_window: DEFAULT_NEIGHBORHOOD_STABILITY_WINDOW,
            depth_lower_window: DEFAULT_DEPTH_LOWER_WINDOW,
            phase_stability_window: DEFAULT_PHASE_STABILITY_WINDOW,
        }
    }
}

impl KademliaConfig {
    /// Create with custom total target peers, preserving all other limits.
    pub fn with_total_target(mut self, total: usize) -> Self {
        self.limits = self.limits.with_total_target(total);
        self
    }

    /// Create with custom nominal minimum per bin, preserving all other limits.
    pub fn with_nominal(mut self, nominal: usize) -> Self {
        self.limits = self.limits.with_nominal(nominal);
        self
    }

    /// Create with custom inbound headroom.
    pub fn with_inbound_headroom(mut self, headroom: usize) -> Self {
        self.limits = self.limits.with_inbound_headroom(headroom);
        self
    }

    /// Set the neighborhood stability window, preserving all other fields.
    ///
    /// The window is how long the neighborhood (bins at and above the current
    /// depth) must stay saturated without the depth moving before
    /// `ReadinessSnapshot::is_neighborhood_ready` reports true. Any depth
    /// change or saturation dip restarts the clock. Pull-syncing is the
    /// intended consumer: it should start against a settled neighborhood,
    /// not a transiently well-connected one.
    pub fn with_neighborhood_stability_window(mut self, window: Duration) -> Self {
        self.neighborhood_stability_window = window;
        self
    }

    /// Set the stability window for publishing a marginal depth lowering.
    ///
    /// Raising depth is always applied immediately: over-connection is
    /// harmless and the trim floor protects the climb. Lowering is applied
    /// immediately only when the saturation deficit across the bins below
    /// the published depth exceeds one peer (real capacity loss). A
    /// single-peer deficit, the signature of one churning frontier peer, is
    /// instead held for this window and published only if the recomputed
    /// depth stays below the published depth for the whole window.
    pub fn with_depth_lower_window(mut self, window: Duration) -> Self {
        self.depth_lower_window = window;
        self
    }

    /// Set the phase-machine stability window: how long depth must hold
    /// still, with a saturated neighborhood, before the topology phase
    /// reports `Stable`.
    pub fn with_phase_stability_window(mut self, window: Duration) -> Self {
        self.phase_stability_window = window;
        self
    }
}

#[cfg(test)]
impl KademliaConfig {
    /// Create with custom depth-aware limits.
    pub(crate) fn with_limits(limits: DepthAwareLimits) -> Self {
        Self {
            limits,
            ..Default::default()
        }
    }

    /// Set the per-bin bootstrap fill target used while `depth == 0`.
    pub(crate) fn with_bootstrap_target(mut self, target: usize) -> Self {
        self.limits = self.limits.with_bootstrap_target(target);
        self
    }

    /// Set the per-bin oversaturation level (trim floor and minimum
    /// inbound ceiling).
    pub(crate) fn with_oversaturation_peers(mut self, oversaturation: usize) -> Self {
        self.limits = self.limits.with_oversaturation_peers(oversaturation);
        self
    }

    /// Set the saturation threshold (production threads it from the spec).
    pub(crate) fn with_saturation(mut self, saturation: usize) -> Self {
        self.limits = self.limits.with_saturation(saturation);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_primitives::{Bin, NeighborhoodDepth};

    fn b(n: u8) -> Bin {
        Bin::new(n).expect("valid bin")
    }

    fn d(n: u8) -> NeighborhoodDepth {
        NeighborhoodDepth::new(b(n))
    }

    #[test]
    fn test_config_default() {
        let config = KademliaConfig::default();
        assert_eq!(config.limits.nominal(), 3);
        assert_eq!(config.limits.total_target(), 160);
    }

    #[test]
    fn test_with_total_target() {
        let config = KademliaConfig::default().with_total_target(256);
        assert_eq!(config.limits.total_target(), 256);
        assert_eq!(config.limits.nominal(), 3);
    }

    #[test]
    fn test_with_nominal() {
        let config = KademliaConfig::default().with_nominal(5);
        assert_eq!(config.limits.nominal(), 5);
        assert_eq!(config.limits.total_target(), 160);
    }

    #[test]
    fn test_builders_preserve_sibling_fields() {
        // Builders must not silently reset other limits to defaults.
        let config = KademliaConfig::default()
            .with_inbound_headroom(8)
            .with_total_target(64)
            .with_nominal(5);
        assert_eq!(config.limits.total_target(), 64);
        assert_eq!(config.limits.nominal(), 5);
        // Headroom survived the later builders: bin 7 at depth 8 has target
        // 64 * 8 / 36 = 14, ceiling = max(14 + 8, 18) = 22.
        assert!(config.limits.should_accept_inbound(b(7), d(8), 21));
        assert!(!config.limits.should_accept_inbound(b(7), d(8), 22));
    }

    #[test]
    fn test_with_inbound_headroom() {
        let config = KademliaConfig::default().with_inbound_headroom(8);
        // Headroom is internal; verify depth-aware behavior works
        // At depth 8, bin 7 target = 35. With headroom 8, ceiling = 43.
        // At target + 7 = 42, should still accept inbound
        assert!(config.limits.should_accept_inbound(b(7), d(8), 35 + 7));
        // At target + 8 = 43, should not accept
        assert!(!config.limits.should_accept_inbound(b(7), d(8), 35 + 8));
    }

    #[test]
    fn test_with_neighborhood_stability_window() {
        let config = KademliaConfig::default();
        assert_eq!(
            config.neighborhood_stability_window,
            Duration::from_secs(30)
        );

        let config = config.with_neighborhood_stability_window(Duration::from_secs(5));
        assert_eq!(config.neighborhood_stability_window, Duration::from_secs(5));
        // Sibling fields are preserved.
        assert_eq!(config.limits.total_target(), 160);
    }

    #[test]
    fn test_with_limits() {
        let custom = DepthAwareLimits::new(200, 4);
        let config = KademliaConfig::with_limits(custom);
        assert_eq!(config.limits.total_target(), 200);
        assert_eq!(config.limits.nominal(), 4);
    }
}
