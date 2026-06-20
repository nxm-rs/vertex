//! Size-driven storage-radius dynamics.
//!
//! The reserve's [`StorageRadius`] is derived from its occupancy relative to
//! capacity, not configured statically. [`derive_radius`] is the pure decision
//! function; [`RadiusController::apply`] threads the eviction I/O and the
//! [`SettableRadius`] write around it.
//!
//! The rule over an occupancy snapshot:
//!
//! - **Grow** (radius + 1) when `total > capacity`, after shedding the
//!   shallowest in-radius bin. A deeper radius narrows responsibility, shedding
//!   load. Capped at [`MAX_PO`], beyond which the node is over-provisioned and
//!   the loop surfaces [`RadiusOutcome::AtCeiling`]. This is a hard constraint
//!   and takes precedence over shrink.
//! - **Shrink** (radius - 1) when within-radius occupancy is below
//!   [`shrink_threshold`], the node is not mid-sync, and the radius is above its
//!   configured floor. A shallower radius widens responsibility so an
//!   under-filled node pulls in more chunks. Syncing must be idle because an
//!   in-progress sync is still filling the reserve, so its low occupancy is not
//!   spare capacity.
//! - **Hold** otherwise.
//!
//! The reserve seam: [`occupancy_of`] reads `count`/`count_in`/`capacity`/
//! `storage_radius` from a [`ReserveStore`]; grow sheds via
//! [`ReserveStore::evict_from_bin`]; the committed radius is written back
//! through [`SettableRadius`]. [`DbReserve`](crate::DbReserve) implements both.

use nectar_primitives::{Bin, MAX_PO, ProximityOrder};
use vertex_swarm_api::{ReserveStore, SettableRadius, StorageRadius, SwarmError, SwarmResult};

/// A single radius-adjustment decision derived from reserve occupancy.
///
/// Shrink/grow move the radius by exactly one step so the loop converges
/// monotonically rather than overshooting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadiusDecision {
    /// Leave the radius unchanged.
    Hold,
    /// Under-filled and idle: lower the radius to the contained value.
    Shrink(StorageRadius),
    /// Over capacity: raise the radius to the contained value after shedding the
    /// shallowest bin.
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
/// The multiply-then-divide form keeps the same truncation if the proportion is
/// ever retuned.
#[inline]
#[must_use]
pub const fn shrink_threshold(capacity: u64) -> u64 {
    // saturating_mul keeps the policy total at capacities that would overflow `* 5`.
    capacity.saturating_mul(5) / 10
}

/// Derive the next radius adjustment from a reserve-occupancy snapshot.
///
/// Pure and total. `minimum_radius` is the floor the shrink rule will not cross;
/// `syncing_idle` is false while a historical sync is in flight, which suppresses
/// shrinking. Over-capacity (grow) is checked first as a hard constraint; shrink
/// is considered only when within capacity.
#[must_use]
pub fn derive_radius(
    occ: ReserveOccupancy,
    minimum_radius: StorageRadius,
    syncing_idle: bool,
) -> RadiusDecision {
    if occ.total > occ.capacity {
        return match step_up(occ.radius) {
            Some(next) => RadiusDecision::Grow(next),
            None => RadiusDecision::Hold, // already at the ceiling
        };
    }

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
/// The within-radius sum walks at most `MAX_PO - radius + 1` bins, each an
/// O(log n + matches) cursor count.
pub fn occupancy_of<R: ReserveStore + ?Sized>(reserve: &R) -> SwarmResult<ReserveOccupancy> {
    let radius = reserve.storage_radius();
    let total = reserve.count()?;
    let mut within_radius = 0u64;
    for po in radius.get()..=MAX_PO {
        // `po` is a valid bin index by construction; fall back to the deepest bin
        // defensively rather than unwrapping.
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

/// Drives the grow loop to convergence over a pure shed function, keeping radius
/// stepping and ceiling detection separate from the eviction I/O.
///
/// `shed(radius)` evicts the bin at the current radius boundary (`bin == radius`,
/// the shallowest bin that leaves responsibility when the radius rises) and
/// returns the new total. The loop stops once the total is within capacity, and
/// steps up at most `MAX_PO - start` times so it always terminates.
///
/// The returned radius is always one deeper than the deepest bin shed:
/// responsibility is bins at or deeper than the radius, so shedding bin `K` and
/// moving the boundary to `K + 1` are one step. This matches the single-step
/// [`RadiusDecision::Grow`] convention of `radius + 1`.
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
                // The radius becomes `next` regardless of whether this shed
                // sufficed, because bin `radius` is gone either way.
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
/// Holds the configured floor; the radius itself lives in the reserve.
#[derive(Debug, Clone, Copy)]
pub struct RadiusController {
    /// The configured minimum radius the shrink rule will not cross.
    minimum_radius: StorageRadius,
}

impl RadiusController {
    #[must_use]
    pub const fn new(minimum_radius: StorageRadius) -> Self {
        Self { minimum_radius }
    }

    #[must_use]
    pub const fn minimum_radius(&self) -> StorageRadius {
        self.minimum_radius
    }

    #[must_use]
    pub fn decide(&self, occ: ReserveOccupancy, syncing_idle: bool) -> RadiusDecision {
        derive_radius(occ, self.minimum_radius, syncing_idle)
    }

    /// [`occupancy_of`] + [`Self::decide`] in one call for the live loop.
    pub fn decide_for<R: ReserveStore + ?Sized>(
        &self,
        reserve: &R,
        syncing_idle: bool,
    ) -> SwarmResult<RadiusDecision> {
        Ok(self.decide(occupancy_of(reserve)?, syncing_idle))
    }

    /// Decide and apply one radius adjustment against a live reserve, returning
    /// the radius now committed.
    ///
    /// Reads the occupancy, runs [`derive_radius`], performs the implied I/O, and
    /// commits the resulting radius through [`SettableRadius`]:
    ///
    /// - [`Hold`](RadiusDecision::Hold): commit nothing, return the current radius.
    /// - [`Shrink`](RadiusDecision::Shrink): widening evicts nothing, so commit the
    ///   shallower radius directly.
    /// - [`Grow`](RadiusDecision::Grow): run [`grow_to_capacity`], shedding the
    ///   boundary bin via [`evict_from_bin`](ReserveStore::evict_from_bin) and
    ///   stepping the radius until within capacity (or at the ceiling), then commit.
    ///   Eviction precedes the commit so the reserve never advertises a narrower
    ///   radius than its contents justify.
    ///
    /// Returns a [`SwarmResult`] because the grow path performs reserve I/O.
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
                // A reserve I/O error inside the closure is captured and surfaced
                // after the loop rather than panicking through the pure driver.
                let mut shed_err: Option<SwarmError> = None;
                let outcome = grow_to_capacity(occ.radius, occ.capacity, occ.total, |radius| {
                    if shed_err.is_some() {
                        // A prior shed failed; report still over capacity so the
                        // loop makes no further claim.
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

/// The maximum entries [`RadiusController::apply`] sheds from one bin per
/// [`evict_from_bin`](ReserveStore::evict_from_bin) call.
///
/// Bounds the per-bin eviction transaction so a pathologically populated bin
/// does not stall the loop; the loop re-runs on the next tick to drain any
/// remainder.
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
        // Under-filled but a sync is in flight: low occupancy is not spare
        // capacity, so hold.
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
        // (radius -> 10), total 13 -> 10 within. Converged radius is 10.
        assert_eq!(shed_at, vec![8, 9], "sheds the boundary bin each step");
        assert_eq!(out, RadiusOutcome::WithinCapacity(r(10)));
        // The deepest shed bin is exactly radius - 1. This is consensus-observable:
        // storage_radius feeds committedDepth, so an off-by-one diverges on chain.
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

    /// Differential oracle: replay a recorded `(occupancy, syncing_idle)` trace
    /// through [`derive_radius`] and assert the radius trajectory matches a
    /// reference trace step for step.
    #[test]
    #[ignore = "differential oracle: needs a recorded reference trace"]
    fn radius_oracle() {
        unimplemented!("differential oracle trace not yet recorded");
    }
}
