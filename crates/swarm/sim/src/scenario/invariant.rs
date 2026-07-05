//! Reusable invariant checks over live routing statistics.
//!
//! Checks run against [`RoutingStats`] snapshots read from a real topology
//! handle between world steps; the single-threaded scheduler guarantees the
//! snapshot observes settled state.

use vertex_swarm_topology::RoutingStats;

type Check = Box<dyn Fn(&RoutingStats) -> Result<(), String>>;

/// Named invariants a sim test registers once and checks between steps.
#[derive(Default)]
pub struct Invariants {
    checks: Vec<(&'static str, Check)>,
}

impl Invariants {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a custom named check.
    pub fn register(
        mut self,
        name: &'static str,
        check: impl Fn(&RoutingStats) -> Result<(), String> + 'static,
    ) -> Self {
        self.checks.push((name, Box::new(check)));
        self
    }

    /// The published depth never sits below `floor`.
    pub fn depth_floor(self, floor: u8) -> Self {
        self.register("depth-floor", move |stats| {
            if stats.depth >= floor {
                Ok(())
            } else {
                Err(format!(
                    "published depth {} sits below the floor {floor}",
                    stats.depth
                ))
            }
        })
    }

    /// Per bin the active phase counter equals the connected set, and the
    /// connected total equals the per-bin sum: no peer is double-counted
    /// and no lifecycle path leaks a phase counter.
    pub fn phase_counters_consistent(self) -> Self {
        self.register("phase-counters", |stats| {
            for bin in &stats.bins {
                if bin.active != bin.connected {
                    return Err(format!(
                        "bin {}: active counter {} disagrees with connected {}",
                        bin.bin, bin.active, bin.connected
                    ));
                }
            }
            let bin_sum: usize = stats.bins.iter().map(|bin| bin.connected).sum();
            if bin_sum != stats.connected_peers_total {
                return Err(format!(
                    "connected total {} disagrees with the per-bin sum {bin_sum}",
                    stats.connected_peers_total
                ));
            }
            Ok(())
        })
    }

    /// Every bin below the published depth keeps its allocation target at or
    /// above `saturation` (the allocation floor).
    pub fn saturation_floor(self, saturation: usize) -> Self {
        self.register("saturation-floor", move |stats| {
            for bin in &stats.bins {
                if bin.bin < stats.depth && bin.target < saturation {
                    return Err(format!(
                        "below-depth bin {} target {} dropped under saturation {saturation}",
                        bin.bin, bin.target
                    ));
                }
            }
            Ok(())
        })
    }

    /// Run every check, collecting violations as `name: message` lines.
    pub fn check(&self, stats: &RoutingStats) -> Result<(), Vec<String>> {
        let violations: Vec<String> = self
            .checks
            .iter()
            .filter_map(|(name, check)| check(stats).err().map(|msg| format!("{name}: {msg}")))
            .collect();
        if violations.is_empty() {
            Ok(())
        } else {
            Err(violations)
        }
    }

    /// Panic with every violation and the seed that replays the run.
    #[allow(clippy::panic)]
    pub fn assert(&self, stats: &RoutingStats, seed: u64) {
        if let Err(violations) = self.check(stats) {
            panic!("invariants violated (seed={seed}): {violations:#?}");
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use vertex_swarm_topology::BinStats;

    fn bin(bin: u8, connected: usize, active: usize, target: usize) -> BinStats {
        BinStats {
            bin,
            connected,
            known: connected,
            dialing: 0,
            handshaking: 0,
            active,
            target,
            ceiling: target.saturating_add(2),
            nominal: 4,
        }
    }

    fn stats(depth: u8, bins: Vec<BinStats>) -> RoutingStats {
        let connected_peers_total = bins.iter().map(|b| b.connected).sum();
        RoutingStats {
            bins,
            depth,
            known_peers_total: 0,
            connected_peers_total,
        }
    }

    #[test]
    fn healthy_snapshot_passes() {
        let invariants = Invariants::new()
            .depth_floor(1)
            .phase_counters_consistent()
            .saturation_floor(8);
        let snapshot = stats(1, vec![bin(0, 8, 8, 8), bin(1, 3, 3, usize::MAX)]);
        assert!(invariants.check(&snapshot).is_ok());
    }

    #[test]
    fn depth_floor_catches_a_collapse() {
        let invariants = Invariants::new().depth_floor(2);
        let snapshot = stats(1, vec![bin(0, 8, 8, 8)]);
        let violations = invariants.check(&snapshot).unwrap_err();
        assert_eq!(violations.len(), 1);
        assert!(violations.iter().any(|v| v.starts_with("depth-floor:")));
    }

    #[test]
    fn phase_counters_catch_a_leak() {
        let invariants = Invariants::new().phase_counters_consistent();
        // Leaked active counter: one more active than connected.
        let snapshot = stats(0, vec![bin(0, 3, 4, 8)]);
        let violations = invariants.check(&snapshot).unwrap_err();
        assert!(violations.iter().any(|v| v.starts_with("phase-counters:")));
    }

    #[test]
    fn phase_counters_catch_a_double_count() {
        let invariants = Invariants::new().phase_counters_consistent();
        let mut snapshot = stats(0, vec![bin(0, 3, 3, 8)]);
        snapshot.connected_peers_total = 4;
        let violations = invariants.check(&snapshot).unwrap_err();
        assert!(violations.iter().any(|v| v.contains("per-bin sum")));
    }

    #[test]
    fn saturation_floor_catches_a_starved_target() {
        let invariants = Invariants::new().saturation_floor(8);
        let snapshot = stats(2, vec![bin(0, 8, 8, 8), bin(1, 8, 8, 4)]);
        let violations = invariants.check(&snapshot).unwrap_err();
        assert!(
            violations
                .iter()
                .any(|v| v.starts_with("saturation-floor:"))
        );
    }

    #[test]
    fn custom_checks_register() {
        let invariants = Invariants::new().register("known-nonzero", |stats| {
            if stats.known_peers_total > 0 {
                Ok(())
            } else {
                Err("no known peers".into())
            }
        });
        let snapshot = stats(0, vec![]);
        assert!(invariants.check(&snapshot).is_err());
    }
}
