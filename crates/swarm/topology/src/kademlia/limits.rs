//! Depth-aware bin allocation with linear tapering.
//!
//! Allocates more peers to higher bins (closer neighbors) where peers are
//! scarcer and more valuable for retrieval parallelization.

/// Default minimum peers per bin (floor for depth calculation).
const DEFAULT_NOMINAL: usize = 3;

/// Default total connected peer target.
const DEFAULT_TOTAL_TARGET: usize = 160;

/// Default ceiling for inbound connections above target.
pub(crate) const DEFAULT_INBOUND_HEADROOM: usize = 4;

/// Depth-aware peer allocation with linear tapering across Kademlia bins.
///
/// Stateless: callers provide depth explicitly to avoid dual-source-of-truth bugs.
#[derive(Debug, Clone)]
pub(crate) struct DepthAwareLimits {
    total_target: usize,
    /// Minimum peers per bin.
    nominal: usize,
    inbound_headroom: usize,
}

impl Default for DepthAwareLimits {
    fn default() -> Self {
        Self::new(DEFAULT_TOTAL_TARGET, DEFAULT_NOMINAL)
    }
}

#[allow(dead_code)]
impl DepthAwareLimits {
    /// Create with total target and nominal minimum per bin.
    pub(crate) fn new(total_target: usize, nominal: usize) -> Self {
        Self {
            total_target,
            nominal,
            inbound_headroom: DEFAULT_INBOUND_HEADROOM,
        }
    }

    /// Create with custom inbound headroom.
    pub(crate) fn with_inbound_headroom(mut self, headroom: usize) -> Self {
        self.inbound_headroom = headroom;
        self
    }

    /// Minimum peers per bin (floor).
    pub(crate) fn nominal(&self) -> usize {
        self.nominal
    }

    /// Extra headroom for accepting inbound connections above target.
    pub(crate) fn inbound_headroom(&self) -> usize {
        self.inbound_headroom
    }

    /// Total target peers across all bins.
    pub(crate) fn total_target(&self) -> usize {
        self.total_target
    }

    /// Target for bin at depth. Returns `usize::MAX` for neighborhood bins (>= depth).
    pub(crate) fn target(&self, bin: u8, depth: u8) -> usize {
        if depth == 0 {
            // No depth yet - use nominal for all bins
            return self.nominal;
        }

        if bin >= depth {
            // Neighborhood: connect to ALL available
            usize::MAX
        } else {
            // Linear taper: bin i gets weight (i + 1)
            // weight_sum = depth × (depth + 1) / 2
            let weight = bin as usize + 1;
            let weight_sum = (depth as usize) * (depth as usize + 1) / 2;
            let allocated = self.total_target.saturating_mul(weight) / weight_sum;
            allocated.max(self.nominal)
        }
    }

    /// Check if bin needs more peers at specified depth.
    pub(crate) fn needs_more(&self, bin: u8, depth: u8, connected: usize) -> bool {
        let target = self.target(bin, depth);
        if target == usize::MAX {
            // Neighborhood: always want more if available
            true
        } else {
            connected < target
        }
    }

    /// Deficit from target at specified depth.
    pub(crate) fn deficit(&self, bin: u8, depth: u8, connected: usize) -> usize {
        let target = self.target(bin, depth);
        if target == usize::MAX {
            // Neighborhood: report large deficit to prioritize
            1000usize.saturating_sub(connected)
        } else {
            target.saturating_sub(connected)
        }
    }

    /// Surplus above target at specified depth (0 if at or below target).
    pub(crate) fn surplus(&self, bin: u8, depth: u8, connected: usize) -> usize {
        let target = self.target(bin, depth);
        if target == usize::MAX {
            0
        } else {
            connected.saturating_sub(target)
        }
    }

    /// Target + inbound headroom (max before rejecting inbound). `usize::MAX` for neighborhood.
    pub(crate) fn ceiling(&self, bin: u8, depth: u8) -> usize {
        let target = self.target(bin, depth);
        if target == usize::MAX {
            usize::MAX
        } else {
            target + self.inbound_headroom
        }
    }

    /// Check if bin should accept inbound (allows headroom above target).
    pub(crate) fn should_accept_inbound(&self, bin: u8, depth: u8, connected: usize) -> bool {
        let target = self.target(bin, depth);
        if target == usize::MAX {
            // Neighborhood: always accept
            true
        } else {
            connected < target + self.inbound_headroom
        }
    }

    /// Expected available peers in bin (exponential estimate from uniform distribution).
    pub(crate) fn expected_available(&self, bin: u8, depth: u8) -> usize {
        if depth == 0 || bin >= depth {
            // Neighborhood bins or no depth: sparse, return nominal
            self.nominal
        } else {
            // Exponential growth as bin decreases
            let shift = (depth - bin).min(20); // Cap to avoid overflow
            self.nominal.saturating_mul(1 << shift)
        }
    }

    /// Total expected peers across all bins below depth.
    pub(crate) fn total_expected_at_depth(&self, depth: u8) -> usize {
        if depth == 0 {
            return 0;
        }
        // Sum of geometric series: nominal × (2 + 4 + 8 + ... + 2^depth)
        // = nominal × 2 × (2^depth - 1)
        let two_to_depth = 1usize << depth.min(20);
        self.nominal
            .saturating_mul(2)
            .saturating_mul(two_to_depth.saturating_sub(1))
    }

    /// Estimate depth from known peer distribution (highest bin with >= nominal peers).
    pub(crate) fn estimate_depth_from_known(&self, known_bin_sizes: &[usize]) -> u8 {
        // Find highest bin with >= nominal known peers
        for (po, &count) in known_bin_sizes.iter().enumerate().rev() {
            if count >= self.nominal {
                return po as u8;
            }
        }
        0
    }

    /// Estimate depth by projecting known peer distribution to higher bins.
    pub(crate) fn estimate_depth_projected(&self, known_bin_sizes: &[usize]) -> u8 {
        // Find a reference bin with significant population
        let mut ref_bin = 0u8;
        let mut ref_count = 0usize;

        for (po, &count) in known_bin_sizes.iter().enumerate() {
            if count > ref_count {
                ref_bin = po as u8;
                ref_count = count;
            }
        }

        if ref_count < self.nominal {
            return 0;
        }

        // Project population to higher bins using exponential decay
        // In Kademlia, each higher bin has ~half the peers
        let mut estimated_depth = ref_bin;
        let mut projected = ref_count;

        while projected >= self.nominal && estimated_depth < 31 {
            estimated_depth += 1;
            projected /= 2;
        }

        // Back up to last bin with sufficient projected population
        if projected < self.nominal && estimated_depth > 0 {
            estimated_depth -= 1;
        }

        estimated_depth
    }

    /// Effective depth: max(connected_depth, estimated_depth) for bootstrap.
    pub(crate) fn effective_depth(&self, connected_depth: u8, known_bin_sizes: &[usize]) -> u8 {
        let estimated = self.estimate_depth_from_known(known_bin_sizes);
        connected_depth.max(estimated)
    }

    /// Target using effective depth (for allocation with known peer distribution).
    pub(crate) fn target_effective(
        &self,
        bin: u8,
        connected_depth: u8,
        known_bin_sizes: &[usize],
    ) -> usize {
        self.target(bin, self.effective_depth(connected_depth, known_bin_sizes))
    }

    /// Check if we need more peers using effective depth.
    pub(crate) fn needs_more_effective(
        &self,
        bin: u8,
        connected_depth: u8,
        connected: usize,
        known_bin_sizes: &[usize],
    ) -> bool {
        self.needs_more(
            bin,
            self.effective_depth(connected_depth, known_bin_sizes),
            connected,
        )
    }

    /// Generate allocation table for debugging/metrics.
    pub(crate) fn allocation_table(&self, depth: u8) -> Vec<(u8, usize)> {
        (0..32).map(|bin| (bin, self.target(bin, depth))).collect()
    }
}

/// Snapshot of limits at a specific depth for TOCTOU-safe candidate selection.
pub(crate) struct LimitsSnapshot {
    pub depth: u8,
    limits: DepthAwareLimits,
}

#[allow(dead_code)]
impl LimitsSnapshot {
    pub(crate) fn capture(limits: &DepthAwareLimits, depth: u8) -> Self {
        Self {
            depth,
            limits: limits.clone(),
        }
    }

    pub(crate) fn target(&self, bin: u8) -> usize {
        self.limits.target(bin, self.depth)
    }

    pub(crate) fn is_neighborhood(&self, bin: u8) -> bool {
        bin >= self.depth
    }

    pub(crate) fn needs_more(&self, bin: u8, connected: usize) -> bool {
        self.limits.needs_more(bin, self.depth, connected)
    }

    pub(crate) fn deficit(&self, bin: u8, connected: usize) -> usize {
        self.limits.deficit(bin, self.depth, connected)
    }

    pub(crate) fn surplus(&self, bin: u8, connected: usize) -> usize {
        self.limits.surplus(bin, self.depth, connected)
    }

    pub(crate) fn should_accept_inbound(&self, bin: u8, connected: usize) -> bool {
        self.limits
            .should_accept_inbound(bin, self.depth, connected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_linear_taper_depth_8() {
        let limits = DepthAwareLimits::new(160, 3);

        // Weight sum for depth 8: 8 × 9 / 2 = 36
        // Bin 7: 160 × 8 / 36 = 35.5 → 35
        // Bin 0: 160 × 1 / 36 = 4.4 → max(4, 3) = 4

        assert_eq!(limits.target(7, 8), 35);
        assert_eq!(limits.target(6, 8), 31); // 160 × 7 / 36 = 31.1
        assert_eq!(limits.target(0, 8), 4); // 160 × 1 / 36 = 4.4

        // Neighborhood (bin >= depth) returns MAX
        assert_eq!(limits.target(8, 8), usize::MAX);
        assert_eq!(limits.target(10, 8), usize::MAX);
    }

    #[test]
    fn test_needs_more() {
        let limits = DepthAwareLimits::new(160, 3);

        // Bin 7 target = 35
        assert!(limits.needs_more(7, 8, 0));
        assert!(limits.needs_more(7, 8, 34));
        assert!(!limits.needs_more(7, 8, 35));
        assert!(!limits.needs_more(7, 8, 40));

        // Neighborhood always needs more
        assert!(limits.needs_more(8, 8, 1000));
    }

    #[test]
    fn test_should_accept_inbound() {
        let limits = DepthAwareLimits::new(160, 3).with_inbound_headroom(4);

        // Bin 7 target = 35, ceiling = 35 + 4 = 39
        assert!(limits.should_accept_inbound(7, 8, 35));
        assert!(limits.should_accept_inbound(7, 8, 38));
        assert!(!limits.should_accept_inbound(7, 8, 39));

        // Neighborhood always accepts
        assert!(limits.should_accept_inbound(8, 8, 1000));
    }

    #[test]
    fn test_expected_available() {
        let limits = DepthAwareLimits::new(160, 3);

        // Bin 7: 3 × 2^1 = 6
        assert_eq!(limits.expected_available(7, 8), 6);
        // Bin 6: 3 × 2^2 = 12
        assert_eq!(limits.expected_available(6, 8), 12);
        // Bin 0: 3 × 2^8 = 768
        assert_eq!(limits.expected_available(0, 8), 768);

        // Neighborhood returns nominal
        assert_eq!(limits.expected_available(8, 8), 3);
    }

    #[test]
    fn test_total_expected() {
        let limits = DepthAwareLimits::new(160, 3);

        // Depth 8: 3 × 2 × (256 - 1) = 1530
        assert_eq!(limits.total_expected_at_depth(8), 1530);

        // Depth 10: 3 × 2 × (1024 - 1) = 6138
        assert_eq!(limits.total_expected_at_depth(10), 6138);
    }

    #[test]
    fn test_snapshot_consistency() {
        let limits = DepthAwareLimits::new(160, 3);

        let snapshot = LimitsSnapshot::capture(&limits, 8);

        // Snapshot captures depth at creation time
        assert_eq!(snapshot.depth, 8);
        assert_eq!(snapshot.target(7), 35); // Uses depth 8 calculation

        // A different snapshot at depth 10 has different targets
        let snapshot10 = LimitsSnapshot::capture(&limits, 10);
        assert_eq!(snapshot10.depth, 10);

        // Original snapshot unchanged
        assert_eq!(snapshot.depth, 8);
        assert_eq!(snapshot.target(7), 35);
    }

    #[test]
    fn test_deficit() {
        let limits = DepthAwareLimits::new(160, 3);

        // Bin 7 target at depth 8 = 35
        assert_eq!(limits.deficit(7, 8, 0), 35);
        assert_eq!(limits.deficit(7, 8, 20), 15);
        assert_eq!(limits.deficit(7, 8, 35), 0);
        assert_eq!(limits.deficit(7, 8, 40), 0);
    }

    #[test]
    fn test_zero_depth() {
        let limits = DepthAwareLimits::new(160, 3);

        // All bins return nominal when depth is 0
        assert_eq!(limits.target(0, 0), 3);
        assert_eq!(limits.target(7, 0), 3);
        assert_eq!(limits.target(31, 0), 3);
    }

    #[test]
    fn test_various_total_targets() {
        // Light client
        let light = DepthAwareLimits::new(32, 2);
        assert!(light.target(7, 8) < 10);

        // Robust retrieval
        let robust = DepthAwareLimits::new(256, 4);
        assert!(robust.target(7, 8) > 50);
    }

    #[test]
    fn test_estimate_depth_from_known() {
        let limits = DepthAwareLimits::new(160, 3);

        // No known peers -> depth 0
        let empty: Vec<usize> = vec![0; 32];
        assert_eq!(limits.estimate_depth_from_known(&empty), 0);

        // Known peers in low bins only -> depth based on highest populated
        let mut known = vec![0; 32];
        known[0] = 100; // bin 0: 100 peers
        known[1] = 50; // bin 1: 50 peers
        known[2] = 20; // bin 2: 20 peers
        known[3] = 10; // bin 3: 10 peers
        known[4] = 5; // bin 4: 5 >= nominal
        known[5] = 2; // bin 5: 2 < nominal
        assert_eq!(limits.estimate_depth_from_known(&known), 4);

        // Known peers in higher bins -> higher estimated depth
        known[6] = 3; // bin 6: exactly nominal
        assert_eq!(limits.estimate_depth_from_known(&known), 6);

        known[7] = 3; // bin 7: exactly nominal
        assert_eq!(limits.estimate_depth_from_known(&known), 7);
    }

    #[test]
    fn test_effective_depth() {
        let limits = DepthAwareLimits::new(160, 3);

        // No known peers, connected depth 0 -> effective depth = 0
        let empty: Vec<usize> = vec![0; 32];
        assert_eq!(limits.effective_depth(0, &empty), 0);

        // Known peers estimate depth 5, connected depth 0
        let mut known = vec![0; 32];
        known[0] = 100;
        known[5] = 3;
        let estimated = limits.estimate_depth_from_known(&known);
        assert_eq!(estimated, 5);
        assert_eq!(limits.effective_depth(0, &known), 5);

        // Connected depth higher than estimated -> use connected
        assert_eq!(limits.effective_depth(7, &known), 7);
    }

    #[test]
    fn test_target_effective() {
        let limits = DepthAwareLimits::new(160, 3);

        // With no known peers and connected depth 0, target_effective returns nominal
        let empty: Vec<usize> = vec![0; 32];
        assert_eq!(limits.target_effective(5, 0, &empty), 3);

        // With known peers estimating depth 5, get proper allocation
        let mut known = vec![0; 32];
        known[0] = 100;
        known[5] = 3;
        // At depth 5: bin 4 should have linear-tapered target
        let target = limits.target_effective(4, 0, &known);
        assert!(target > 3); // Should be more than nominal due to tapering
    }

    #[test]
    fn test_estimate_depth_projected() {
        let limits = DepthAwareLimits::new(160, 3);

        // 768 peers in bin 0 suggests depth ~8 (3 * 2^8 = 768)
        let mut known = vec![0; 32];
        known[0] = 768;
        // Projects: bin 1 = 384, bin 2 = 192, ..., bin 7 = 6, bin 8 = 3
        let projected = limits.estimate_depth_projected(&known);
        assert!((7..=9).contains(&projected), "projected = {}", projected);

        // Fewer peers in bin 0 suggests lower depth
        known[0] = 24; // 24 / 2 = 12, /2 = 6, /2 = 3 -> depth ~3
        let projected = limits.estimate_depth_projected(&known);
        assert!((2..=4).contains(&projected), "projected = {}", projected);
    }

    #[test]
    fn test_surplus_below_target() {
        let limits = DepthAwareLimits::new(160, 3);

        // Bin 7 target at depth 8 = 35, connected < target
        assert_eq!(limits.surplus(7, 8, 0), 0);
        assert_eq!(limits.surplus(7, 8, 20), 0);
        assert_eq!(limits.surplus(7, 8, 34), 0);
    }

    #[test]
    fn test_surplus_at_target() {
        let limits = DepthAwareLimits::new(160, 3);

        // Bin 7 target at depth 8 = 35, connected == target
        assert_eq!(limits.surplus(7, 8, 35), 0);
    }

    #[test]
    fn test_surplus_above_target() {
        let limits = DepthAwareLimits::new(160, 3);

        // Bin 7 target at depth 8 = 35, connected > target
        assert_eq!(limits.surplus(7, 8, 40), 5);
        assert_eq!(limits.surplus(7, 8, 36), 1);
    }

    #[test]
    fn test_surplus_neighborhood_always_zero() {
        let limits = DepthAwareLimits::new(160, 3);

        // Neighborhood bins (>= depth) have target MAX, surplus always 0
        assert_eq!(limits.surplus(8, 8, 1000), 0);
        assert_eq!(limits.surplus(10, 8, 500), 0);
    }

    #[test]
    fn test_surplus_at_depth_zero() {
        let limits = DepthAwareLimits::new(160, 3);
        // depth = 0: all bins use nominal (3) as target

        assert_eq!(limits.surplus(0, 0, 2), 0);
        assert_eq!(limits.surplus(0, 0, 3), 0);
        assert_eq!(limits.surplus(0, 0, 5), 2);
        assert_eq!(limits.surplus(7, 0, 10), 7);
    }

    #[test]
    fn test_surplus_at_depth_transition() {
        let limits = DepthAwareLimits::new(160, 3);

        // At depth 5: bin 4 target = 160 × 5 / 15 = 53
        assert_eq!(limits.surplus(4, 5, 40), 0);
        assert_eq!(limits.surplus(4, 5, 53), 0);
        assert_eq!(limits.surplus(4, 5, 60), 7);

        // At depth 8: bin 4 target = 160 × 5 / 36 = 22
        assert_eq!(limits.surplus(4, 8, 40), 18);
        assert_eq!(limits.surplus(4, 8, 22), 0);
    }

    #[test]
    fn test_snapshot_surplus() {
        let limits = DepthAwareLimits::new(160, 3);

        let snapshot = LimitsSnapshot::capture(&limits, 8);

        // Bin 7 target = 35
        assert_eq!(snapshot.surplus(7, 30), 0);
        assert_eq!(snapshot.surplus(7, 35), 0);
        assert_eq!(snapshot.surplus(7, 40), 5);

        // Neighborhood
        assert_eq!(snapshot.surplus(8, 1000), 0);
    }
}
