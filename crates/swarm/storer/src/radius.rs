//! Size-driven storage-radius dynamics.
//!
//! The reserve's [`StorageRadius`] is not a static configuration value: it is
//! derived from the reserve's own occupancy relative to its capacity, and that
//! derived radius (plus a node-configured capacity-doubling addend) is the
//! consensus-committed depth a redistribution round samples and commits on
//! chain. This module is the clean-room, hand-derived specification of that
//! derivation, expressed as a pure decision function so it can be unit-tested
//! in isolation and reused by both the live eviction-control loop and the
//! conformance tests.
//!
//! # Clean-room specification
//!
//! Derived from the Swarm reserve worker's intent (size-driven radius growth
//! and shrink), encoded here from first principles rather than transcribed from
//! the reference implementation. The rule, stated as a state machine over an
//! occupancy snapshot:
//!
//! - **Within-capacity is the invariant.** The reserve must never sit above its
//!   capacity once the control loop has converged: `total <= capacity`. The
//!   grow rule restores this invariant whenever it is violated.
//! - **Shrink threshold.** A reserve is considered comfortably under-filled when
//!   its within-radius population sits below half its capacity. The `threshold`
//!   is `capacity * 5 / 10` (equivalently `capacity / 2`). Below this the radius
//!   may shrink; at or above it the radius holds.
//! - **Shrink (radius decrease).** When the within-radius count is below the
//!   threshold *and* the node is not actively syncing historical content
//!   (`syncRate == 0`) *and* the radius is above its configured minimum, the
//!   radius decreases by one. This widens responsibility (a shallower radius
//!   admits more of the address space) so an under-utilised node pulls in more
//!   chunks. The minimum-radius floor prevents a freshly started or sparsely
//!   populated node from collapsing its radius to zero. Syncing must be idle
//!   because an in-progress sync is still filling the reserve, so its momentary
//!   low occupancy is not evidence of spare long-term capacity.
//! - **Grow (radius increase).** When the reserve exceeds capacity, the node
//!   sheds its furthest (shallowest-bin) chunks via unreserve/eviction and, if
//!   shedding the current shallowest bin is not enough to get back within
//!   capacity, increases the radius by one and repeats. A deeper radius narrows
//!   responsibility (fewer addresses qualify), shedding load. The radius is
//!   capped at the deepest bin ([`MAX_PO`]): a node that is still over capacity
//!   at the maximum radius is genuinely over-provisioned and the control loop
//!   surfaces [`RadiusOutcome::AtCeiling`].
//!
//! The derivation ([`derive_radius`]) is *pure*: it consumes occupancy counts
//! and produces a [`RadiusDecision`]. Applying a decision (evicting a bin,
//! committing the new radius) is [`RadiusController::apply`]'s job, which threads
//! the eviction I/O and the [`SettableRadius`] write around the pure rule; see
//! the [`RadiusController`] driver and the documented [reserve seam](#reserve-seam).
//!
//! # Differential oracle (planned)
//!
//! This module is the *hand-derived* specification: the rules above are encoded
//! and tested against their own documented behaviour. A bee differential oracle
//! (replaying a recorded sequence of `(occupancy, sync state)` snapshots through
//! both this controller and the reference reserve worker, and asserting the
//! radius trajectory matches step for step) is the planned follow-up conformance
//! check. It is intentionally *not* part of this car: it needs a recorded oracle
//! trace and a harness that are owned separately, and folding it in here would
//! couple the pure policy to a fixture. The seam for it is the pure
//! [`derive_radius`] function, which the oracle harness can drive directly. See
//! the `radius_oracle_is_planned_followup` ignored test, which documents the
//! exact comparison the oracle will perform.
//!
//! # Reserve seam
//!
//! The controller reads three quantities from the reserve and writes one back:
//!
//! - reserve population within the current radius
//!   ([`ReserveOccupancy::within_radius`]),
//! - total reserve population ([`ReserveOccupancy::total`]),
//! - capacity ([`ReserveOccupancy::capacity`]),
//! - and, on grow, the per-bin eviction primitive
//!   ([`ReserveStore::evict_from_bin`]).
//!
//! The reads are exactly the existing [`ReserveStore`] surface
//! (`count`/`count_in`/`capacity`/`storage_radius`/`evict_from_bin`); the write
//! is the [`SettableRadius`] extension trait. [`DbReserve`](crate::DbReserve)
//! implements both, so wiring the controller needs no new reserve verbs.
//! [`occupancy_of`] builds the snapshot from any [`ReserveStore`], and
//! [`RadiusController::apply`] reads it, applies the decision (shedding through
//! [`evict_from_bin`](ReserveStore::evict_from_bin) on grow) and commits the
//! derived radius through [`SettableRadius::set_storage_radius`]. #391 only wires
//! the loop that calls `apply`; the runtime mutability already lives here.

use nectar_primitives::{Bin, MAX_PO, ProximityOrder};
use vertex_swarm_api::{ReserveStore, SettableRadius, StorageRadius, SwarmError, SwarmResult};

/// A single radius-adjustment decision derived from reserve occupancy.
///
/// The output of the pure derivation. Holding is the common case; shrink/grow
/// move the radius by exactly one step so the control loop converges
/// monotonically rather than overshooting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadiusDecision {
    /// The radius is well-matched to occupancy; leave it unchanged.
    Hold,
    /// The reserve is under-filled and idle; widen responsibility by lowering
    /// the radius one step (to the contained value).
    Shrink(StorageRadius),
    /// The reserve is over capacity; narrow responsibility by raising the
    /// radius one step (to the contained value), after shedding the shallowest
    /// bin.
    Grow(StorageRadius),
}

/// The terminal outcome of running the grow loop to convergence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadiusOutcome {
    /// The reserve is within capacity at the (possibly increased) radius.
    WithinCapacity(StorageRadius),
    /// The reserve is still over capacity at the deepest bin; the node is
    /// over-provisioned and cannot shed further by widening the radius.
    AtCeiling(StorageRadius),
}

/// A snapshot of reserve occupancy the radius derivation consumes.
///
/// Counts are per *stamped entry* (distinct `(batchID, stampIndex, address)`),
/// matching the consensus reserve-size definition, not per content address.
/// The within-radius count is the population in bins at or deeper than the
/// current radius; it is the quantity the shrink rule tests against the
/// capacity threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReserveOccupancy {
    /// Entries in bins at or deeper than the current radius.
    pub within_radius: u64,
    /// Total entries across all bins.
    pub total: u64,
    /// The reserve's capacity, in stamped entries.
    pub capacity: u64,
    /// The current storage radius.
    pub radius: StorageRadius,
}

/// The capacity threshold below which the radius may shrink: `capacity * 5 / 10`.
///
/// The multiply-then-divide form mirrors the protocol's `capacity * 5 / 10`
/// exactly (same truncation), and avoids the temptation to read `capacity / 2`
/// as a different fraction if the proportion is ever retuned.
#[inline]
#[must_use]
pub const fn shrink_threshold(capacity: u64) -> u64 {
    // saturating_mul guards the (astronomically large) capacity that would
    // overflow `* 5`; the reserve never approaches it, but the policy stays
    // total rather than panicking in a debug build.
    capacity.saturating_mul(5) / 10
}

/// Derive the next radius adjustment from a reserve-occupancy snapshot.
///
/// Pure and total. `minimum_radius` is the configured floor the shrink rule
/// will not cross; `syncing_idle` is true when no historical sync is in flight
/// (a sync in progress, `syncRate != 0`, suppresses shrinking, since low
/// occupancy then reflects an unfinished fill rather than spare capacity).
///
/// Precedence: over-capacity (grow) is checked first because it is a hard
/// constraint (the reserve must not exceed capacity); shrink is a soft
/// optimisation considered only when within capacity.
#[must_use]
pub fn derive_radius(
    occ: ReserveOccupancy,
    minimum_radius: StorageRadius,
    syncing_idle: bool,
) -> RadiusDecision {
    // Hard constraint first: shed load if over capacity by narrowing.
    if occ.total > occ.capacity {
        return match step_up(occ.radius) {
            Some(next) => RadiusDecision::Grow(next),
            None => RadiusDecision::Hold, // already at the ceiling
        };
    }

    // Soft optimisation: widen responsibility when comfortably under-filled and
    // not mid-sync, never below the configured floor.
    if syncing_idle
        && occ.within_radius < shrink_threshold(occ.capacity)
        && occ.radius > minimum_radius
        && let Some(next) = step_down(occ.radius)
    {
        return RadiusDecision::Shrink(next);
    }

    RadiusDecision::Hold
}

/// The radius one step deeper, or `None` if already at the deepest bin.
#[inline]
#[must_use]
fn step_up(radius: StorageRadius) -> Option<StorageRadius> {
    let raw = radius.get();
    if raw >= MAX_PO {
        return None;
    }
    Bin::try_from(raw + 1).ok().map(StorageRadius::new)
}

/// The radius one step shallower, or `None` if already at zero.
#[inline]
#[must_use]
fn step_down(radius: StorageRadius) -> Option<StorageRadius> {
    let raw = radius.get();
    if raw == 0 {
        return None;
    }
    Bin::try_from(raw - 1).ok().map(StorageRadius::new)
}

/// Build a [`ReserveOccupancy`] snapshot from a live [`ReserveStore`].
///
/// Reads the within-radius population (the sum of `count_in` over bins at or
/// deeper than the current radius), the total population, the capacity and the
/// radius. This is the read half of the [reserve seam](self#reserve-seam): the
/// controller decides from the snapshot, the live loop applies the decision.
///
/// The within-radius sum walks at most `MAX_PO - radius + 1` bins, each an
/// O(log n + matches) cursor count, so the snapshot is cheap relative to a full
/// scan. A `total` already counted by the reserve is reused rather than
/// re-summed.
pub fn occupancy_of<R: ReserveStore + ?Sized>(reserve: &R) -> SwarmResult<ReserveOccupancy> {
    let radius = reserve.storage_radius();
    let total = reserve.count()?;
    let mut within_radius = 0u64;
    for po in radius.get()..=MAX_PO {
        // `po` ranges over a valid bin index by construction (radius.get() and
        // MAX_PO are both valid), so the conversion cannot fail; fall back to
        // the deepest bin defensively rather than unwrapping.
        let bin = ProximityOrder::new(po).unwrap_or(ProximityOrder::MAX);
        within_radius = within_radius.saturating_add(reserve.count_in(bin)?);
    }
    Ok(ReserveOccupancy {
        within_radius,
        total,
        capacity: reserve.capacity(),
        radius,
    })
}

/// Drives the grow loop to convergence over a pure shed function.
///
/// The live wiring passes a closure that sheds the shallowest in-radius bin via
/// [`ReserveStore::evict_from_bin`] and reports the resulting total. This keeps
/// the convergence logic (radius stepping, ceiling detection) pure and testable
/// while leaving the I/O to the caller. The loop steps the radius up at most
/// `MAX_PO - start` times, so it always terminates.
///
/// `shed` is invoked as `shed(radius)`: it should evict the bin at the current
/// radius boundary (`bin == radius`), which is the shallowest bin that falls out
/// of responsibility the moment the radius rises by one, and return the new
/// total entry count. The loop stops as soon as the total is within capacity,
/// returning the radius reached.
///
/// # Radius accounting
///
/// Responsibility is the set of bins *at or deeper than* the radius
/// ([`ReserveStore::storage_radius`] semantics), so shedding bin `K` and raising
/// the radius are one step: the node sheds bin `K` precisely because it is no
/// longer responsible for it once the boundary moves to `K + 1`. The returned
/// radius is therefore always one deeper than the deepest bin that was shed,
/// matching the single-step [`derive_radius`] convention where
/// [`RadiusDecision::Grow`] carries `radius + 1`. Equivalently: increment the
/// radius, then evict the bins now shallower than it.
pub fn grow_to_capacity<F>(
    start: StorageRadius,
    capacity: u64,
    mut total: u64,
    mut shed: F,
) -> RadiusOutcome
where
    F: FnMut(StorageRadius) -> u64,
{
    let mut radius = start;
    while total > capacity {
        match step_up(radius) {
            Some(next) => {
                // Shed the bin at the current boundary; it falls out of
                // responsibility as the radius rises to `next`. The new radius
                // is `next` regardless of whether this shed sufficed, because
                // bin `radius` is gone either way.
                total = shed(radius);
                radius = next;
                if total <= capacity {
                    return RadiusOutcome::WithinCapacity(radius);
                }
            }
            None => return RadiusOutcome::AtCeiling(radius),
        }
    }
    RadiusOutcome::WithinCapacity(radius)
}

/// A thin stateful driver around [`derive_radius`] for the live control loop.
///
/// Holds the configured floor; the radius itself lives in the reserve. The
/// driver is deliberately tiny: it exists so the loop has one place to apply
/// policy and so the policy is covered by tests independently of the
/// redb-backed reserve.
#[derive(Debug, Clone, Copy)]
pub struct RadiusController {
    /// The configured minimum radius the shrink rule will not cross.
    minimum_radius: StorageRadius,
}

impl RadiusController {
    /// Construct a controller with the given minimum-radius floor.
    #[must_use]
    pub const fn new(minimum_radius: StorageRadius) -> Self {
        Self { minimum_radius }
    }

    /// The configured minimum radius.
    #[must_use]
    pub const fn minimum_radius(&self) -> StorageRadius {
        self.minimum_radius
    }

    /// Decide the next radius adjustment for the given occupancy.
    #[must_use]
    pub fn decide(&self, occ: ReserveOccupancy, syncing_idle: bool) -> RadiusDecision {
        derive_radius(occ, self.minimum_radius, syncing_idle)
    }

    /// Decide the next radius adjustment for a live reserve, reading its
    /// occupancy through the [reserve seam](self#reserve-seam).
    ///
    /// A convenience over [`occupancy_of`] + [`Self::decide`] for the live loop;
    /// the pure two-step form remains available for tests and the oracle.
    pub fn decide_for<R: ReserveStore + ?Sized>(
        &self,
        reserve: &R,
        syncing_idle: bool,
    ) -> SwarmResult<RadiusDecision> {
        Ok(self.decide(occupancy_of(reserve)?, syncing_idle))
    }

    /// Decide *and apply* one radius adjustment against a live reserve, returning
    /// the radius now committed.
    ///
    /// This is the write half of the [reserve seam](self#reserve-seam) and the
    /// single entry the live eviction-control loop (#391) calls: it reads the
    /// occupancy, runs [`derive_radius`], performs the I/O the decision implies,
    /// and commits the resulting radius through [`SettableRadius`] so subsequent
    /// [`storage_radius`](ReserveStore::storage_radius) reads observe it. Before
    /// this existed the dynamics were architecturally orphaned: there was no write
    /// target for the derived radius, so it could never change at runtime.
    ///
    /// The decision maps to action as follows:
    ///
    /// - [`Hold`](RadiusDecision::Hold): commit nothing, return the current
    ///   radius unchanged.
    /// - [`Shrink`](RadiusDecision::Shrink): widening responsibility evicts
    ///   nothing (the node simply admits more of the address space), so this
    ///   commits the shallower radius directly.
    /// - [`Grow`](RadiusDecision::Grow): the reserve is over capacity, so this
    ///   runs [`grow_to_capacity`], shedding the boundary bin via
    ///   [`evict_from_bin`](ReserveStore::evict_from_bin) and stepping the radius
    ///   until the reserve is within capacity (or the ceiling is hit), then
    ///   commits the converged radius. Eviction precedes the commit so the reserve
    ///   never advertises a narrower radius than its contents justify.
    ///
    /// `syncing_idle` carries the same meaning as in [`derive_radius`]. Returns a
    /// [`SwarmResult`] because the grow path performs reserve I/O.
    pub fn apply<R: SettableRadius + ?Sized>(
        &self,
        reserve: &R,
        syncing_idle: bool,
    ) -> SwarmResult<StorageRadius> {
        let occ = occupancy_of(reserve)?;
        match self.decide(occ, syncing_idle) {
            RadiusDecision::Hold => Ok(occ.radius),
            RadiusDecision::Shrink(next) => {
                reserve.set_storage_radius(next);
                Ok(next)
            }
            RadiusDecision::Grow(_) => {
                // The grow loop sheds the boundary bin and steps the radius until
                // within capacity; thread its eviction through `evict_from_bin`.
                // A reserve I/O error inside the closure is captured and surfaced
                // after the loop rather than panicking through the pure driver.
                let mut shed_err: Option<SwarmError> = None;
                let outcome = grow_to_capacity(occ.radius, occ.capacity, occ.total, |radius| {
                    if shed_err.is_some() {
                        // A prior shed failed; stop shedding (report the count as
                        // still over capacity so the loop makes no further claim).
                        return occ.total;
                    }
                    match reserve.evict_from_bin(radius.bin(), BIN_EVICT_MAX) {
                        Ok(_) => match reserve.count() {
                            Ok(total) => total,
                            Err(e) => {
                                shed_err = Some(e);
                                occ.total
                            }
                        },
                        Err(e) => {
                            shed_err = Some(e);
                            occ.total
                        }
                    }
                });
                if let Some(e) = shed_err {
                    return Err(e);
                }
                let next = match outcome {
                    RadiusOutcome::WithinCapacity(r) | RadiusOutcome::AtCeiling(r) => r,
                };
                reserve.set_storage_radius(next);
                Ok(next)
            }
        }
    }
}

/// The maximum entries [`RadiusController::apply`] sheds from one bin in a single
/// [`evict_from_bin`](ReserveStore::evict_from_bin) call.
///
/// Bounds the per-bin eviction transaction so a pathologically populated bin
/// does not stall the control loop in one giant transaction; the loop re-runs on
/// the next tick to drain any remainder. Matches the batch-eviction bound the
/// expiry sweep uses.
pub const BIN_EVICT_MAX: u64 = 10_000;

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test assertions over known-bounds inputs"
)]
mod tests {
    use super::*;

    fn r(n: u8) -> StorageRadius {
        StorageRadius::new(Bin::try_from(n).unwrap())
    }

    fn occ(within: u64, total: u64, cap: u64, radius: u8) -> ReserveOccupancy {
        ReserveOccupancy {
            within_radius: within,
            total,
            capacity: cap,
            radius: r(radius),
        }
    }

    #[test]
    fn threshold_is_five_tenths_of_capacity() {
        assert_eq!(shrink_threshold(0), 0);
        assert_eq!(shrink_threshold(1), 0);
        assert_eq!(shrink_threshold(10), 5);
        assert_eq!(shrink_threshold(11), 5);
        // Matches the protocol's capacity * 5 / 10 for representative sizes.
        for c in [0u64, 1, 2, 9, 10, 1000, 4_194_304] {
            assert_eq!(shrink_threshold(c), c * 5 / 10);
        }
    }

    // --- below-threshold decrease ------------------------------------------

    #[test]
    fn shrinks_when_underfilled_idle_and_above_floor() {
        // within (4) < threshold (5), idle, radius 8 > floor 0 => shrink to 7.
        let d = derive_radius(occ(4, 8, 10, 8), r(0), true);
        assert_eq!(d, RadiusDecision::Shrink(r(7)));
    }

    #[test]
    fn holds_when_at_or_above_threshold() {
        // within (5) == threshold (5): not below, so hold.
        assert_eq!(
            derive_radius(occ(5, 8, 10, 8), r(0), true),
            RadiusDecision::Hold
        );
        // within (6) > threshold: hold.
        assert_eq!(
            derive_radius(occ(6, 8, 10, 8), r(0), true),
            RadiusDecision::Hold
        );
    }

    #[test]
    fn does_not_shrink_while_syncing() {
        // Under-filled but a sync is in flight (syncRate != 0): low occupancy is
        // not spare capacity, so hold.
        assert_eq!(
            derive_radius(occ(1, 8, 10, 8), r(0), false),
            RadiusDecision::Hold
        );
    }

    #[test]
    fn does_not_shrink_below_floor() {
        // Under-filled and idle, but radius already at the configured floor.
        assert_eq!(
            derive_radius(occ(1, 8, 10, 5), r(5), true),
            RadiusDecision::Hold
        );
        // One above the floor: shrink down to the floor exactly.
        assert_eq!(
            derive_radius(occ(1, 8, 10, 6), r(5), true),
            RadiusDecision::Shrink(r(5))
        );
    }

    #[test]
    fn does_not_shrink_at_zero_radius() {
        // Floor at zero, radius at zero: step_down has nowhere to go.
        assert_eq!(
            derive_radius(occ(0, 0, 10, 0), r(0), true),
            RadiusDecision::Hold
        );
    }

    // --- over-capacity increase --------------------------------------------

    #[test]
    fn grows_when_over_capacity() {
        // total (12) > capacity (10): grow one step regardless of sync state.
        assert_eq!(
            derive_radius(occ(12, 12, 10, 8), r(0), true),
            RadiusDecision::Grow(r(9))
        );
        assert_eq!(
            derive_radius(occ(12, 12, 10, 8), r(0), false),
            RadiusDecision::Grow(r(9))
        );
    }

    #[test]
    fn over_capacity_takes_precedence_over_shrink() {
        // Within-radius low (would shrink) but total over capacity: grow wins.
        let d = derive_radius(occ(1, 12, 10, 8), r(0), true);
        assert_eq!(d, RadiusDecision::Grow(r(9)));
    }

    #[test]
    fn grow_holds_at_ceiling() {
        // Over capacity but already at the deepest bin: cannot widen further.
        assert_eq!(
            derive_radius(occ(50, 50, 10, MAX_PO), r(0), true),
            RadiusDecision::Hold
        );
    }

    #[test]
    fn within_capacity_at_boundary_holds() {
        // total == capacity is within capacity; no grow.
        assert_eq!(
            derive_radius(occ(10, 10, 10, 8), r(0), true),
            RadiusDecision::Hold
        );
    }

    // --- grow loop convergence ---------------------------------------------

    #[test]
    fn grow_loop_converges_within_capacity() {
        // Start over capacity; each shed removes 3 entries. capacity 10,
        // total 16 -> 13 -> 10 (within at the second shed).
        let mut remaining = 16u64;
        let mut shed_at: Vec<u8> = Vec::new();
        let out = grow_to_capacity(r(8), 10, 16, |radius| {
            shed_at.push(radius.get());
            remaining = remaining.saturating_sub(3);
            remaining
        });
        // Shed bin 8 (radius -> 9), total 16 -> 13 still over; shed bin 9
        // (radius -> 10), total 13 -> 10 within. Responsibility is bins >= radius
        // and bins 8 and 9 are now evicted, so the converged radius is 10, one
        // deeper than the deepest bin shed.
        assert_eq!(shed_at, vec![8, 9], "sheds the boundary bin each step");
        assert_eq!(out, RadiusOutcome::WithinCapacity(r(10)));
        // The converged radius is the lowest bin still in responsibility:
        // responsibility is bins >= radius, every shed bin is now below it, and
        // the deepest shed bin (9) is exactly radius - 1. This is the consensus
        // property: storage_radius feeds committedDepth, so an off-by-one here
        // diverges on chain.
        let radius = match out {
            RadiusOutcome::WithinCapacity(rdx) | RadiusOutcome::AtCeiling(rdx) => rdx.get(),
        };
        assert_eq!(
            radius,
            *shed_at.last().unwrap() + 1,
            "radius is one deeper than the deepest evicted bin"
        );
        for shed in &shed_at {
            assert!(
                *shed < radius,
                "every shed bin {shed} is shallower than the radius {radius} (out of responsibility)"
            );
        }
    }

    #[test]
    fn grow_loop_reports_ceiling_when_cannot_shed_enough() {
        // Shedding never reduces below capacity: must hit the ceiling.
        let out = grow_to_capacity(r(MAX_PO - 1), 10, 100, |_r| 100);
        assert_eq!(out, RadiusOutcome::AtCeiling(r(MAX_PO)));
    }

    #[test]
    fn grow_loop_no_op_when_already_within_capacity() {
        let out = grow_to_capacity(r(8), 10, 5, |_r| unreachable!("should not shed"));
        assert_eq!(out, RadiusOutcome::WithinCapacity(r(8)));
    }

    // --- controller --------------------------------------------------------

    #[test]
    fn controller_threads_floor() {
        let c = RadiusController::new(r(4));
        assert_eq!(c.minimum_radius(), r(4));
        assert_eq!(c.decide(occ(1, 8, 10, 4), true), RadiusDecision::Hold);
        assert_eq!(
            c.decide(occ(1, 8, 10, 5), true),
            RadiusDecision::Shrink(r(4))
        );
    }

    /// The bee differential oracle is a planned follow-up: it will replay a
    /// recorded `(occupancy, syncing_idle)` trace through both [`derive_radius`]
    /// and bee's reserve worker and assert the radius trajectory matches step
    /// for step. The fixture and harness are owned separately; this test
    /// documents the comparison and the seam (drive [`derive_radius`] directly)
    /// rather than asserting a green result that does not run.
    #[test]
    #[ignore = "bee differential oracle: needs a recorded reference trace (planned follow-up)"]
    fn radius_oracle_is_planned_followup() {
        // Placeholder shape of the oracle step:
        //   for (snapshot, idle, expected) in recorded_trace() {
        //       assert_eq!(derive_radius(snapshot, floor, idle), expected);
        //   }
        unimplemented!("differential oracle trace not yet recorded");
    }
}
