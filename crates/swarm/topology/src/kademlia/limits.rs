//! Depth-aware bin allocation with linear tapering.
//!
//! Allocates more peers to higher bins (closer neighbors) where peers are
//! scarcer and more valuable for retrieval parallelization.

use vertex_swarm_api::DEFAULT_SATURATION_PEERS;
use vertex_swarm_primitives::{Bin, NeighborhoodDepth};

/// Default minimum peers per bin for depth estimation. A bin is considered
/// populated once it holds this many peers. Note this is NOT the taper floor:
/// allocation targets are additionally floored at `saturation` so every
/// balanced bin can reach the depth frontier (see [`DepthAwareLimits::target`]).
const DEFAULT_NOMINAL: usize = 3;

/// Default total connected-peer target across all bins. The linear taper
/// distributes this budget, weighting bins closer to the neighborhood more
/// heavily where peers are scarcer and more valuable for retrieval.
const DEFAULT_TOTAL_TARGET: usize = 160;

/// Default headroom for inbound connections above a bin's target. Accepting a
/// few extra inbound per bin keeps reachable peers connected without letting any
/// single bin grow unbounded.
pub(crate) const DEFAULT_INBOUND_HEADROOM: usize = 4;

/// Default per-bin fill target while no neighborhood is established
/// (`depth == 0`). Before depth climbs above zero every bin is filled
/// aggressively toward this level so bins reach the saturation frontier
/// quickly and depth can climb. Bounded (not `usize::MAX`) so a node that
/// has not yet established a neighborhood cannot be flooded by inbound
/// connections. This knob controls only how aggressively to fill while
/// blind; the retention band at any depth is
/// [`DEFAULT_OVERSATURATION_PEERS`].
pub(crate) const DEFAULT_BOOTSTRAP_TARGET: usize = 18;

/// Default per-bin oversaturation level: the floor below which trimming
/// never evicts ([`DepthAwareLimits::surplus`]) and the minimum inbound
/// ceiling ([`DepthAwareLimits::ceiling`]) at any depth.
///
/// Together with the saturation floor this defines the per-bin band: dial
/// toward `max(taper, saturation)`, accept inbound up to this level, trim
/// only above it. This knob controls only how many peers above target a
/// bin may retain; the bootstrap fill level is
/// [`DEFAULT_BOOTSTRAP_TARGET`]. The two coincide at 18 so default
/// behaviour matches the live network's oversaturation level.
pub(crate) const DEFAULT_OVERSATURATION_PEERS: usize = 18;

/// Depth-aware peer allocation with linear tapering across Kademlia bins.
///
/// Stateless: callers provide depth explicitly to avoid dual-source-of-truth bugs.
///
/// # Depth coupling invariants
///
/// Neighborhood depth is recomputed from connected-peer bin counts against
/// this type's `saturation` (threaded from `SwarmSpec::saturation_peers()`
/// at behaviour construction): the shallowest bin below saturation caps the
/// depth. Allocation and trimming must therefore never hold or cut a
/// balanced bin below saturation, or depth deadlocks at that bin with no
/// error signal. Both properties are structural, not asserted:
///
/// - every allocation target (bootstrap and taper) is floored at
///   `saturation` inside [`Self::target`], so no configuration can produce
///   a target below it,
/// - trimming ([`Self::surplus`]) only reclaims peers above
///   `max(target, oversaturation)`, never toward a dial target, where the
///   effective oversaturation level is clamped to at least
///   `bootstrap_target` and `saturation` inside [`Self::oversaturation`],
///   so no configuration can trim below the bootstrap fill or the depth
///   frontier.
#[derive(Debug, Clone)]
pub(crate) struct DepthAwareLimits {
    total_target: usize,
    /// Minimum peers per bin for depth estimation.
    nominal: usize,
    inbound_headroom: usize,
    /// Per-bin fill target during bootstrap (`depth == 0`). Controls only
    /// how aggressively to fill while no neighborhood is established; the
    /// retention band is `oversaturation_peers`.
    bootstrap_target: usize,
    /// Per-bin oversaturation level: trim floor and minimum inbound
    /// ceiling at any depth. Read through [`Self::oversaturation`], which
    /// clamps it to at least `bootstrap_target` and `saturation`.
    oversaturation_peers: usize,
    /// Per-bin saturation threshold from `SwarmSpec::saturation_peers()`.
    /// Floors every balanced-bin allocation target so bins can reach the
    /// depth frontier.
    saturation: usize,
}

impl Default for DepthAwareLimits {
    fn default() -> Self {
        Self::new(DEFAULT_TOTAL_TARGET, DEFAULT_NOMINAL)
    }
}

impl DepthAwareLimits {
    /// Create with total target and nominal minimum per bin.
    pub(crate) fn new(total_target: usize, nominal: usize) -> Self {
        Self {
            total_target,
            nominal,
            inbound_headroom: DEFAULT_INBOUND_HEADROOM,
            bootstrap_target: DEFAULT_BOOTSTRAP_TARGET,
            oversaturation_peers: DEFAULT_OVERSATURATION_PEERS,
            saturation: DEFAULT_SATURATION_PEERS as usize,
        }
    }

    /// Create with custom inbound headroom.
    pub(crate) fn with_inbound_headroom(mut self, headroom: usize) -> Self {
        self.inbound_headroom = headroom;
        self
    }

    /// Set the saturation threshold from `SwarmSpec::saturation_peers()`.
    ///
    /// Depth recomputation reads it back via [`Self::saturation`], so
    /// allocation and the depth frontier always agree. Every allocation
    /// target is floored at this value inside [`Self::target`], so no
    /// combination with `bootstrap_target` can deadlock depth.
    pub(crate) fn with_saturation(mut self, saturation: usize) -> Self {
        self.saturation = saturation;
        self
    }

    /// Set the per-bin bootstrap fill target used while `depth == 0`
    /// (production threads it from the connection profile).
    ///
    /// Targets are floored at saturation inside [`Self::target`], so a
    /// bootstrap target below the saturation threshold has no effect on its
    /// own and can never deadlock the depth climb.
    pub(crate) fn with_bootstrap_target(mut self, target: usize) -> Self {
        self.bootstrap_target = target;
        self
    }

    /// Set the total connected-peer dial budget, preserving all other fields.
    pub(crate) fn with_total_target(mut self, total_target: usize) -> Self {
        self.total_target = total_target;
        self
    }

    /// Set the per-bin depth-estimation minimum, preserving all other fields.
    pub(crate) fn with_nominal(mut self, nominal: usize) -> Self {
        self.nominal = nominal;
        self
    }

    /// Per-bin saturation threshold; the depth frontier and all allocation
    /// floors derive from this single value.
    pub(crate) fn saturation(&self) -> usize {
        self.saturation
    }

    /// Effective per-bin oversaturation level: the trim floor in
    /// [`Self::surplus`] and the minimum inbound ceiling in
    /// [`Self::ceiling`].
    ///
    /// Clamped to `max(oversaturation_peers, bootstrap_target, saturation)`
    /// at read time, so the invariants `bootstrap_target <= oversaturation`
    /// and `saturation <= oversaturation` hold for any builder order and
    /// input: trimming can never cut a bin below the level bootstrap
    /// deliberately filled it to, and never below the depth frontier.
    fn oversaturation(&self) -> usize {
        self.oversaturation_peers
            .max(self.bootstrap_target)
            .max(self.saturation)
    }

    /// Minimum peers per bin (floor).
    pub(crate) fn nominal(&self) -> usize {
        self.nominal
    }

    /// Total target peers across all bins.
    pub(crate) fn total_target(&self) -> usize {
        self.total_target
    }

    /// Target for bin at depth. Returns `usize::MAX` for neighborhood bins
    /// (`depth.contains(bin)`).
    pub(crate) fn target(&self, bin: Bin, depth: NeighborhoodDepth) -> usize {
        if depth == NeighborhoodDepth::ZERO {
            // Bootstrap: no neighborhood established yet. Fill every bin
            // aggressively toward `bootstrap_target` so bins reach the
            // saturation frontier quickly and depth can climb. Bounded so a
            // not-yet-established node cannot be flooded by inbound. Floored
            // at saturation so no configuration can pin depth at 0.
            return self.bootstrap_target.max(self.saturation);
        }

        if depth.contains(bin) {
            // Neighborhood: connect to ALL available
            usize::MAX
        } else {
            // Linear taper: bin i gets weight (i + 1)
            // weight_sum = depth × (depth + 1) / 2
            let weight = bin.as_index() + 1;
            let d = depth.get() as usize;
            let weight_sum = d * (d + 1) / 2;
            let allocated = self.total_target.saturating_mul(weight) / weight_sum;
            // Floor at saturation: depth is capped by the shallowest bin below
            // `saturation_peers`, so a target below it would pin depth at that
            // bin forever (the taper alone gives shallow bins as few as
            // total_target / weight_sum peers).
            allocated.max(self.nominal).max(self.saturation)
        }
    }

    /// Check if bin needs more peers at specified depth.
    pub(crate) fn needs_more(&self, bin: Bin, depth: NeighborhoodDepth, connected: usize) -> bool {
        let target = self.target(bin, depth);
        if target == usize::MAX {
            // Neighborhood: always want more if available
            true
        } else {
            connected < target
        }
    }

    /// Deficit from target at specified depth.
    pub(crate) fn deficit(&self, bin: Bin, depth: NeighborhoodDepth, connected: usize) -> usize {
        let target = self.target(bin, depth);
        if target == usize::MAX {
            // Neighborhood: report large deficit to prioritize
            1000usize.saturating_sub(connected)
        } else {
            target.saturating_sub(connected)
        }
    }

    /// Surplus eligible for trimming at the specified depth.
    ///
    /// Trimming reclaims peers only above `max(target, oversaturation)`,
    /// never down to the taper target itself. The taper target is a dial
    /// goal, not an eviction bound: evicting toward it after a depth increase
    /// cuts shallow bins below saturation, which immediately collapses depth
    /// back to the cut bin (observed live: depth 7 -> 0 with bins trimmed to
    /// the exact taper targets).
    pub(crate) fn surplus(&self, bin: Bin, depth: NeighborhoodDepth, connected: usize) -> usize {
        let target = self.target(bin, depth);
        if target == usize::MAX {
            0
        } else {
            // The trim floor is `max(target, oversaturation)`, and
            // `oversaturation()` is itself floored at saturation. So even when a
            // download-churn depth dip momentarily drops the closest bin out of
            // the neighbourhood, trimming never cuts it below saturation: the
            // closest peers retrieval and pushsync route through stay connected
            // while the churn passes.
            connected.saturating_sub(target.max(self.oversaturation()))
        }
    }

    /// Maximum bin population before rejecting inbound. `usize::MAX` for
    /// neighborhood bins.
    ///
    /// `max(target + headroom, oversaturation)`: shallow bins whose dial
    /// target sits at the saturation floor still accept inbound up to the
    /// oversaturation level, so churn erodes them toward it rather than
    /// toward the depth frontier (dial to saturation, accept to
    /// oversaturation, trim above it). This also keeps the ceiling aligned
    /// with the trim floor in [`Self::surplus`].
    pub(crate) fn ceiling(&self, bin: Bin, depth: NeighborhoodDepth) -> usize {
        let target = self.target(bin, depth);
        if target == usize::MAX {
            usize::MAX
        } else {
            (target + self.inbound_headroom).max(self.oversaturation())
        }
    }

    /// Check if bin should accept inbound (up to [`Self::ceiling`]).
    pub(crate) fn should_accept_inbound(
        &self,
        bin: Bin,
        depth: NeighborhoodDepth,
        connected: usize,
    ) -> bool {
        connected < self.ceiling(bin, depth)
    }

    /// Estimate depth from known peer distribution (highest bin with >= nominal peers).
    pub(crate) fn estimate_depth_from_known(&self, known_bin_sizes: &[usize]) -> NeighborhoodDepth {
        // Find highest bin with >= nominal known peers
        for (idx, &count) in known_bin_sizes.iter().enumerate().rev() {
            if count >= self.nominal {
                return NeighborhoodDepth::new(Bin::new(idx as u8).unwrap_or(Bin::MAX));
            }
        }
        NeighborhoodDepth::ZERO
    }

    /// Effective depth: max(connected_depth, estimated_depth) for bootstrap.
    pub(crate) fn effective_depth(
        &self,
        connected_depth: NeighborhoodDepth,
        known_bin_sizes: &[usize],
    ) -> NeighborhoodDepth {
        let estimated = self.estimate_depth_from_known(known_bin_sizes);
        connected_depth.max(estimated)
    }
}

#[cfg(test)]
impl DepthAwareLimits {
    /// Set the per-bin oversaturation level (trim floor and minimum inbound
    /// ceiling).
    ///
    /// The effective level is clamped to at least `bootstrap_target` and
    /// `saturation` at read time ([`Self::oversaturation`]), so a value
    /// below either has no effect on its own.
    pub(crate) fn with_oversaturation_peers(mut self, oversaturation: usize) -> Self {
        self.oversaturation_peers = oversaturation;
        self
    }
    /// Expected available peers in bin (exponential estimate from uniform distribution).
    pub(crate) fn expected_available(&self, bin: Bin, depth: NeighborhoodDepth) -> usize {
        if depth == NeighborhoodDepth::ZERO || depth.contains(bin) {
            // Neighborhood bins or no depth: sparse, return nominal
            self.nominal
        } else {
            // Exponential growth as bin decreases
            let shift = (depth.get() - bin.get()).min(20); // Cap to avoid overflow
            self.nominal.saturating_mul(1 << shift)
        }
    }

    /// Total expected peers across all bins below depth.
    pub(crate) fn total_expected_at_depth(&self, depth: NeighborhoodDepth) -> usize {
        if depth == NeighborhoodDepth::ZERO {
            return 0;
        }
        // Sum of geometric series: nominal × (2 + 4 + 8 + ... + 2^depth)
        // = nominal × 2 × (2^depth - 1)
        let two_to_depth = 1usize << depth.get().min(20);
        self.nominal
            .saturating_mul(2)
            .saturating_mul(two_to_depth.saturating_sub(1))
    }

    /// Estimate depth by projecting known peer distribution to higher bins.
    pub(crate) fn estimate_depth_projected(&self, known_bin_sizes: &[usize]) -> NeighborhoodDepth {
        // Find a reference bin with significant population
        let mut ref_bin = 0u8;
        let mut ref_count = 0usize;

        for (idx, &count) in known_bin_sizes.iter().enumerate() {
            if count > ref_count {
                ref_bin = idx as u8;
                ref_count = count;
            }
        }

        if ref_count < self.nominal {
            return NeighborhoodDepth::ZERO;
        }

        // Project population to higher bins using exponential decay
        // In Kademlia, each higher bin has ~half the peers
        let mut estimated_depth = ref_bin;
        let mut projected = ref_count;

        while projected >= self.nominal && estimated_depth < Bin::MAX.get() {
            estimated_depth += 1;
            projected /= 2;
        }

        // Back up to last bin with sufficient projected population
        if projected < self.nominal && estimated_depth > 0 {
            estimated_depth -= 1;
        }

        NeighborhoodDepth::new(Bin::new(estimated_depth).unwrap_or(Bin::MAX))
    }

    /// Target using effective depth (for allocation with known peer distribution).
    pub(crate) fn target_effective(
        &self,
        bin: Bin,
        connected_depth: NeighborhoodDepth,
        known_bin_sizes: &[usize],
    ) -> usize {
        self.target(bin, self.effective_depth(connected_depth, known_bin_sizes))
    }
}

/// Snapshot of limits at a specific depth for TOCTOU-safe candidate selection.
pub(crate) struct LimitsSnapshot {
    pub depth: NeighborhoodDepth,
    limits: DepthAwareLimits,
}

impl LimitsSnapshot {
    pub(crate) fn capture(limits: &DepthAwareLimits, depth: NeighborhoodDepth) -> Self {
        Self {
            depth,
            limits: limits.clone(),
        }
    }

    pub(crate) fn needs_more(&self, bin: Bin, connected: usize) -> bool {
        self.limits.needs_more(bin, self.depth, connected)
    }

    pub(crate) fn deficit(&self, bin: Bin, connected: usize) -> usize {
        self.limits.deficit(bin, self.depth, connected)
    }
}

#[cfg(test)]
impl LimitsSnapshot {
    pub(crate) fn target(&self, bin: Bin) -> usize {
        self.limits.target(bin, self.depth)
    }

    pub(crate) fn surplus(&self, bin: Bin, connected: usize) -> usize {
        self.limits.surplus(bin, self.depth, connected)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use super::*;

    /// Shorthand to build a Bin in tests.
    fn b(n: u8) -> Bin {
        Bin::new(n).expect("valid bin")
    }

    fn d(n: u8) -> NeighborhoodDepth {
        NeighborhoodDepth::new(b(n))
    }

    #[test]
    fn test_linear_taper_depth_8() {
        let limits = DepthAwareLimits::new(160, 3);

        // Weight sum for depth 8: 8 × 9 / 2 = 36
        // Bin 7: 160 × 8 / 36 = 35.5 → 35
        // Bin 0: 160 × 1 / 36 = 4.4 → floored at saturation (8)

        assert_eq!(limits.target(b(7), d(8)), 35);
        assert_eq!(limits.target(b(6), d(8)), 31); // 160 × 7 / 36 = 31.1
        assert_eq!(limits.target(b(0), d(8)), 8); // taper 4.4, saturation floor

        // Neighborhood (bin >= depth) returns MAX
        assert_eq!(limits.target(b(8), d(8)), usize::MAX);
        assert_eq!(limits.target(b(10), d(8)), usize::MAX);
    }

    #[test]
    fn test_taper_floored_at_saturation_for_all_depths() {
        // Depth is capped by the shallowest bin below saturation, so every
        // balanced-bin target must be at least the saturation threshold or
        // depth deadlocks at that bin.
        let saturation = 8usize;
        let limits = DepthAwareLimits::new(160, 3).with_saturation(saturation);

        for depth in 1..=31u8 {
            for bin in 0..depth {
                let target = limits.target(b(bin), d(depth));
                assert!(
                    target >= saturation,
                    "target {target} < saturation {saturation} at bin {bin}, depth {depth}"
                );
            }
        }
    }

    #[test]
    fn test_trim_floor_never_cuts_below_oversaturation() {
        // Trimming reclaims only above max(target, oversaturation): the
        // taper target is a dial goal, not an eviction bound.
        let limits = DepthAwareLimits::new(160, 3).with_saturation(8);

        // At depth 7 the raw taper gives bin 0 only 160/28 = 5 (floored to 8),
        // but trim must leave up to oversaturation_peers (18) untouched.
        assert_eq!(limits.surplus(b(0), d(7), 18), 0);
        assert_eq!(limits.surplus(b(0), d(7), 26), 8);
        assert_eq!(limits.surplus(b(1), d(7), 25), 7);

        // Targets above the floor trim to the target as before.
        assert_eq!(limits.surplus(b(7), d(8), 40), 5); // target 35
    }

    #[test]
    fn test_trim_floor_follows_oversaturation_not_bootstrap() {
        // The two knobs are distinct: bootstrap_target sets only the
        // depth-0 fill level, oversaturation_peers sets only the retention
        // band (trim floor and minimum inbound ceiling) at any depth.
        let limits = DepthAwareLimits::new(160, 3)
            .with_saturation(8)
            .with_bootstrap_target(10)
            .with_oversaturation_peers(24);

        // Bootstrap fill follows bootstrap_target only, not the band.
        assert_eq!(limits.target(b(0), d(0)), 10);
        assert_eq!(limits.target(b(7), d(0)), 10);

        // Trim floor follows oversaturation_peers (24), not bootstrap (10).
        assert_eq!(limits.surplus(b(0), d(7), 24), 0);
        assert_eq!(limits.surplus(b(0), d(7), 30), 6);

        // Ceiling follows oversaturation_peers where target + headroom is
        // lower: bin 0 at depth 7 has target 8, 8 + 4 = 12 < 24.
        assert_eq!(limits.ceiling(b(0), d(7)), 24);
        // Bin 7 at depth 8 has target 35, 35 + 4 = 39 > 24.
        assert_eq!(limits.ceiling(b(7), d(8)), 39);
    }

    #[test]
    fn test_oversaturation_invariant_under_hostile_inputs() {
        // Oversaturation set below the bootstrap fill: clamped up to
        // bootstrap_target at read time, so trimming can never cut a bin
        // below the level bootstrap deliberately filled it to.
        let limits = DepthAwareLimits::new(160, 3)
            .with_saturation(8)
            .with_oversaturation_peers(2);
        assert_eq!(limits.surplus(b(0), d(7), 18), 0);
        assert_eq!(limits.surplus(b(0), d(7), 20), 2);
        assert_eq!(limits.ceiling(b(0), d(7)), 18);

        // Oversaturation and bootstrap both below saturation: clamped up to
        // saturation regardless of builder order (the clamp is read-time,
        // not setter-time).
        let limits = DepthAwareLimits::new(160, 3)
            .with_oversaturation_peers(1)
            .with_bootstrap_target(1)
            .with_saturation(12);
        assert_eq!(limits.surplus(b(0), d(7), 12), 0);
        assert_eq!(limits.surplus(b(0), d(7), 15), 3);
        // Bin 0 at depth 7: target floored at saturation 12, 12 + 4 = 16.
        assert_eq!(limits.ceiling(b(0), d(7)), 16);

        // Bootstrap raised above the oversaturation level: the band follows
        // bootstrap so the invariant bootstrap_target <= oversaturation
        // cannot be violated.
        let limits = DepthAwareLimits::new(160, 3)
            .with_saturation(8)
            .with_bootstrap_target(30)
            .with_oversaturation_peers(18);
        assert_eq!(limits.target(b(0), d(0)), 30);
        assert_eq!(limits.surplus(b(0), d(7), 30), 0);
        assert_eq!(limits.surplus(b(0), d(7), 33), 3);
        assert_eq!(limits.ceiling(b(0), d(7)), 30);
    }

    /// Connected-per-bin populations from the 2026-06-10 mainnet capture,
    /// taken right before the depth-change trim that collapsed depth 7 -> 0.
    fn mainnet_capture_populations() -> [usize; 32] {
        let mut connected = [0usize; 32];
        for (bin, count) in [
            (0, 26),
            (1, 25),
            (2, 23),
            (3, 16),
            (4, 9),
            (5, 11),
            (6, 13),
            (7, 14),
            (8, 9),
            (9, 3),
        ] {
            connected[bin] = count;
        }
        connected
    }

    /// Apply the depth-change trim to the captured populations and assert
    /// the recomputed depth does not collapse below the climb that
    /// triggered the trim.
    fn assert_trim_preserves_depth(limits: &DepthAwareLimits) {
        let saturation = 8u8;
        let low_watermark = 3u8;
        let depth = d(7);
        let connected = mainnet_capture_populations();

        let before_depth = nectar_primitives::recompute_neighborhood_depth(
            &connected.map(|c| c as u8),
            saturation,
            low_watermark,
        );
        assert!(before_depth.get() >= 7, "capture state supports depth 7");

        // Apply the trim: every balanced bin loses its surplus.
        let mut after = [0u8; 32];
        for (i, slot) in after.iter_mut().enumerate() {
            let bin = b(i as u8);
            let surplus = if depth.contains(bin) {
                0
            } else {
                limits.surplus(bin, depth, connected[i])
            };
            *slot = (connected[i] - surplus) as u8;
        }

        let after_depth =
            nectar_primitives::recompute_neighborhood_depth(&after, saturation, low_watermark);
        // The pre-trim state supports depth 9 via the sparse tail (bins 8-9);
        // the invariant under test is that trimming never cuts depth below
        // the climb the trim was triggered by (7), not that the tail anchor
        // survives, hence the min.
        assert!(
            after_depth.get() >= before_depth.get().min(7),
            "trim collapsed depth: before={} after={}",
            before_depth.get(),
            after_depth.get()
        );
    }

    #[test]
    fn test_trim_preserves_depth_regression() {
        // Regression for the 2026-06-10 mainnet capture: depth climbed to 7,
        // the depth-change trim cut bins 0/1/2 down to the raw taper targets
        // (5/11/17), bin 0 fell below saturation and depth collapsed 7 -> 0
        // permanently. With the trim and taper floors, applying the trim to
        // the captured pre-trim populations must not reduce the recomputed
        // depth.
        let limits = DepthAwareLimits::new(160, 3).with_saturation(8);
        assert_trim_preserves_depth(&limits);
    }

    #[test]
    fn test_trim_preserves_depth_with_lowered_bootstrap() {
        // The connection-profiles use case: a profile lowers the bootstrap
        // fill without touching the retention band. The trim floor follows
        // oversaturation_peers, so the depth-preservation property of the
        // mainnet regression must survive the lowered bootstrap fill.
        let limits = DepthAwareLimits::new(160, 3)
            .with_saturation(8)
            .with_bootstrap_target(8)
            .with_oversaturation_peers(18);
        assert_trim_preserves_depth(&limits);
    }

    #[test]
    fn test_trim_preserves_depth_with_hostile_oversaturation() {
        // Even with the band configured below both bootstrap and
        // saturation, the read-time clamp keeps the trim floor high enough
        // that the captured trim cannot collapse depth.
        let limits = DepthAwareLimits::new(160, 3)
            .with_saturation(8)
            .with_bootstrap_target(8)
            .with_oversaturation_peers(0);
        assert_trim_preserves_depth(&limits);
    }

    #[test]
    fn test_needs_more() {
        let limits = DepthAwareLimits::new(160, 3);

        // Bin 7 target = 35
        assert!(limits.needs_more(b(7), d(8), 0));
        assert!(limits.needs_more(b(7), d(8), 34));
        assert!(!limits.needs_more(b(7), d(8), 35));
        assert!(!limits.needs_more(b(7), d(8), 40));

        // Neighborhood always needs more
        assert!(limits.needs_more(b(8), d(8), 1000));
    }

    #[test]
    fn test_should_accept_inbound() {
        let limits = DepthAwareLimits::new(160, 3).with_inbound_headroom(4);

        // Bin 7 target = 35, ceiling = 35 + 4 = 39
        assert!(limits.should_accept_inbound(b(7), d(8), 35));
        assert!(limits.should_accept_inbound(b(7), d(8), 38));
        assert!(!limits.should_accept_inbound(b(7), d(8), 39));

        // Neighborhood always accepts
        assert!(limits.should_accept_inbound(b(8), d(8), 1000));
    }

    #[test]
    fn test_expected_available() {
        let limits = DepthAwareLimits::new(160, 3);

        // Bin 7: 3 × 2^1 = 6
        assert_eq!(limits.expected_available(b(7), d(8)), 6);
        // Bin 6: 3 × 2^2 = 12
        assert_eq!(limits.expected_available(b(6), d(8)), 12);
        // Bin 0: 3 × 2^8 = 768
        assert_eq!(limits.expected_available(b(0), d(8)), 768);

        // Neighborhood returns nominal
        assert_eq!(limits.expected_available(b(8), d(8)), 3);
    }

    #[test]
    fn test_total_expected() {
        let limits = DepthAwareLimits::new(160, 3);

        // Depth 8: 3 × 2 × (256 - 1) = 1530
        assert_eq!(limits.total_expected_at_depth(d(8)), 1530);

        // Depth 10: 3 × 2 × (1024 - 1) = 6138
        assert_eq!(limits.total_expected_at_depth(d(10)), 6138);
    }

    #[test]
    fn test_snapshot_consistency() {
        let limits = DepthAwareLimits::new(160, 3);

        let snapshot = LimitsSnapshot::capture(&limits, d(8));

        // Snapshot captures depth at creation time
        assert_eq!(snapshot.depth.get(), 8);
        assert_eq!(snapshot.target(b(7)), 35); // Uses depth 8 calculation

        // A different snapshot at depth 10 has different targets
        let snapshot10 = LimitsSnapshot::capture(&limits, d(10));
        assert_eq!(snapshot10.depth.get(), 10);

        // Original snapshot unchanged
        assert_eq!(snapshot.depth.get(), 8);
        assert_eq!(snapshot.target(b(7)), 35);
    }

    #[test]
    fn test_deficit() {
        let limits = DepthAwareLimits::new(160, 3);

        // Bin 7 target at depth 8 = 35
        assert_eq!(limits.deficit(b(7), d(8), 0), 35);
        assert_eq!(limits.deficit(b(7), d(8), 20), 15);
        assert_eq!(limits.deficit(b(7), d(8), 35), 0);
        assert_eq!(limits.deficit(b(7), d(8), 40), 0);
    }

    #[test]
    fn test_zero_depth() {
        let limits = DepthAwareLimits::new(160, 3);

        // All bins fill toward bootstrap_target when depth is 0 (aggressive
        // bootstrap so bins reach the saturation frontier and depth can climb).
        assert_eq!(limits.target(b(0), d(0)), DEFAULT_BOOTSTRAP_TARGET);
        assert_eq!(limits.target(b(7), d(0)), DEFAULT_BOOTSTRAP_TARGET);
        assert_eq!(limits.target(b(31), d(0)), DEFAULT_BOOTSTRAP_TARGET);
    }

    #[test]
    fn test_various_total_targets() {
        // Client with a small connection budget
        let client = DepthAwareLimits::new(32, 2);
        assert!(client.target(b(7), d(8)) < 10);

        // Robust retrieval
        let robust = DepthAwareLimits::new(256, 4);
        assert!(robust.target(b(7), d(8)) > 50);
    }

    #[test]
    fn test_estimate_depth_from_known() {
        let limits = DepthAwareLimits::new(160, 3);

        // No known peers -> depth 0
        let empty: Vec<usize> = vec![0; 32];
        assert_eq!(limits.estimate_depth_from_known(&empty).get(), 0);

        // Known peers in low bins only -> depth based on highest populated
        let mut known = vec![0; 32];
        known[0] = 100; // bin 0: 100 peers
        known[1] = 50; // bin 1: 50 peers
        known[2] = 20; // bin 2: 20 peers
        known[3] = 10; // bin 3: 10 peers
        known[4] = 5; // bin 4: 5 >= nominal
        known[5] = 2; // bin 5: 2 < nominal
        assert_eq!(limits.estimate_depth_from_known(&known).get(), 4);

        // Known peers in higher bins -> higher estimated depth
        known[6] = 3; // bin 6: exactly nominal
        assert_eq!(limits.estimate_depth_from_known(&known).get(), 6);

        known[7] = 3; // bin 7: exactly nominal
        assert_eq!(limits.estimate_depth_from_known(&known).get(), 7);
    }

    #[test]
    fn test_effective_depth() {
        let limits = DepthAwareLimits::new(160, 3);

        // No known peers, connected depth 0 -> effective depth = 0
        let empty: Vec<usize> = vec![0; 32];
        assert_eq!(limits.effective_depth(d(0), &empty).get(), 0);

        // Known peers estimate depth 5, connected depth 0
        let mut known = vec![0; 32];
        known[0] = 100;
        known[5] = 3;
        let estimated = limits.estimate_depth_from_known(&known).get();
        assert_eq!(estimated, 5);
        assert_eq!(limits.effective_depth(d(0), &known).get(), 5);

        // Connected depth higher than estimated -> use connected
        assert_eq!(limits.effective_depth(d(7), &known).get(), 7);
    }

    #[test]
    fn test_target_effective() {
        let limits = DepthAwareLimits::new(160, 3);

        // With no known peers and connected depth 0, target_effective falls
        // back to the depth-0 bootstrap target.
        let empty: Vec<usize> = vec![0; 32];
        assert_eq!(
            limits.target_effective(b(5), d(0), &empty),
            DEFAULT_BOOTSTRAP_TARGET
        );

        // With known peers estimating depth 5, get proper allocation
        let mut known = vec![0; 32];
        known[0] = 100;
        known[5] = 3;
        // At depth 5: bin 4 should have linear-tapered target
        let target = limits.target_effective(b(4), d(0), &known);
        assert!(target > 3); // Should be more than nominal due to tapering
    }

    #[test]
    fn test_estimate_depth_projected() {
        let limits = DepthAwareLimits::new(160, 3);

        // 768 peers in bin 0 suggests depth ~8 (3 * 2^8 = 768)
        let mut known = vec![0; 32];
        known[0] = 768;
        // Projects: bin 1 = 384, bin 2 = 192, ..., bin 7 = 6, bin 8 = 3
        let projected = limits.estimate_depth_projected(&known).get();
        assert!((7..=9).contains(&projected), "projected = {}", projected);

        // Fewer peers in bin 0 suggests lower depth
        known[0] = 24; // 24 / 2 = 12, /2 = 6, /2 = 3 -> depth ~3
        let projected = limits.estimate_depth_projected(&known).get();
        assert!((2..=4).contains(&projected), "projected = {}", projected);
    }

    #[test]
    fn test_surplus_below_target() {
        let limits = DepthAwareLimits::new(160, 3);

        // Bin 7 target at depth 8 = 35, connected < target
        assert_eq!(limits.surplus(b(7), d(8), 0), 0);
        assert_eq!(limits.surplus(b(7), d(8), 20), 0);
        assert_eq!(limits.surplus(b(7), d(8), 34), 0);
    }

    #[test]
    fn test_surplus_at_target() {
        let limits = DepthAwareLimits::new(160, 3);

        // Bin 7 target at depth 8 = 35, connected == target
        assert_eq!(limits.surplus(b(7), d(8), 35), 0);
    }

    #[test]
    fn test_surplus_above_target() {
        let limits = DepthAwareLimits::new(160, 3);

        // Bin 7 target at depth 8 = 35, connected > target
        assert_eq!(limits.surplus(b(7), d(8), 40), 5);
        assert_eq!(limits.surplus(b(7), d(8), 36), 1);
    }

    #[test]
    fn test_surplus_neighborhood_always_zero() {
        let limits = DepthAwareLimits::new(160, 3);

        // Neighborhood bins (>= depth) have target MAX, surplus always 0
        assert_eq!(limits.surplus(b(8), d(8), 1000), 0);
        assert_eq!(limits.surplus(b(10), d(8), 500), 0);
    }

    #[test]
    fn test_surplus_at_depth_zero() {
        let limits = DepthAwareLimits::new(160, 3);
        // depth = 0: all bins fill toward bootstrap_target (18) and the trim
        // floor sits at oversaturation_peers (18); no surplus below it.
        // (Eviction never runs at depth 0 anyway - it scans 0..depth.)

        assert_eq!(limits.surplus(b(0), d(0), 2), 0);
        assert_eq!(limits.surplus(b(0), d(0), 18), 0);
        assert_eq!(limits.surplus(b(0), d(0), 20), 2);
        assert_eq!(limits.surplus(b(7), d(0), 25), 7);
    }

    #[test]
    fn test_surplus_at_depth_transition() {
        let limits = DepthAwareLimits::new(160, 3);

        // At depth 5: bin 4 target = 160 × 5 / 15 = 53
        assert_eq!(limits.surplus(b(4), d(5), 40), 0);
        assert_eq!(limits.surplus(b(4), d(5), 53), 0);
        assert_eq!(limits.surplus(b(4), d(5), 60), 7);

        // At depth 8: bin 4 target = 160 × 5 / 36 = 22
        assert_eq!(limits.surplus(b(4), d(8), 40), 18);
        assert_eq!(limits.surplus(b(4), d(8), 22), 0);
    }

    #[test]
    fn test_snapshot_surplus() {
        let limits = DepthAwareLimits::new(160, 3);

        let snapshot = LimitsSnapshot::capture(&limits, d(8));

        // Bin 7 target = 35
        assert_eq!(snapshot.surplus(b(7), 30), 0);
        assert_eq!(snapshot.surplus(b(7), 35), 0);
        assert_eq!(snapshot.surplus(b(7), 40), 5);

        // Neighborhood
        assert_eq!(snapshot.surplus(b(8), 1000), 0);
    }
}
