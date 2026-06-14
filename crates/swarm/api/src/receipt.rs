//! Custody-receipt depth policy.
//!
//! A pushsync custody receipt is a statement of custody by a node claiming to
//! sit inside the chunk's neighbourhood. Before such a receipt is trusted by an
//! origin uploader or relayed by a forwarder, its signer must be deep enough for
//! the chunk: `PO(signer, chunk) >= required`. The signer overlay is recovered
//! at the pushsync decode boundary (a receipt whose signer cannot be recovered
//! never reaches this module), so the policy here works purely on the recovered
//! signer and the chunk address.
//!
//! This module owns the *policy* (how deep is deep enough). Signer recovery is a
//! wire-tier concern and lives next to the codec; the two are split so the depth
//! rule has no crypto or network-id dependency.

use nectar_primitives::ChunkAddress;
use vertex_swarm_primitives::{NeighborhoodDepth, OverlayAddress, StorageRadius};

/// The recovered signer of a custody receipt is too shallow for the chunk:
/// `PO(signer, chunk) < required`. The chunk never reached the responsible
/// neighbourhood, so the custody claim is fraudulent (or, at best, useless for
/// retrievability) and must be rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("shallow receipt: signer proximity {observed} below required depth {required}")]
pub struct ShallowReceipt {
    /// The required depth the signer had to reach.
    pub required: u8,
    /// The signer's actual proximity order to the chunk address.
    pub observed: u8,
}

impl From<&ShallowReceipt> for &'static str {
    /// The metric label for a shallow-receipt rejection. A static label paired
    /// with the typed `required`/`observed` fields gives metrics a clean
    /// `reason` value without leaking the dynamic depths into the label space.
    fn from(_: &ShallowReceipt) -> Self {
        "shallow_receipt"
    }
}

/// Slack subtracted from the locally observed depth before it becomes the floor.
///
/// Topology views drift: a peer at the genuine edge of the responsible
/// neighbourhood can sit one bin shallower than our own observed depth without
/// being a free-rider. Subtracting a small fixed tolerance from the local floor
/// avoids false-rejecting such an honest, marginally-shallow receipt while still
/// rejecting receipts that are meaningfully outside the neighbourhood. The wire
/// radius can only *raise* the bar above this floor, never lower it.
pub const SHALLOW_RECEIPT_TOLERANCE: u8 = 1;

/// Derive the minimum depth a receipt signer must reach for a chunk.
///
/// The required depth is dynamic: hard-coding it would reject legitimately
/// shallow receipts in a small, young, or sparse neighbourhood, failing valid
/// uploads. The locally observed neighbourhood depth is the trusted authority
/// for how deep the responsible neighbourhood is, and it is the only input an
/// attacker cannot influence, so it sets the *floor* (minus a small
/// [`SHALLOW_RECEIPT_TOLERANCE`] for honest topology drift). The storer's
/// claimed `storage_radius` is attacker-controlled wire data: it is trusted only
/// to *raise* the bar, never to lower it. A storer that declares a deeper radius
/// than our observed depth is binding itself to a stricter custody claim, which
/// we honour; a storer that declares a shallower radius cannot thereby weaken the
/// locally trusted floor, so it cannot launder a shallow free-riding receipt past
/// the check by under-declaring (or zeroing) its radius.
///
/// Hence `required = max(local_depth - tolerance, wire_radius)`. The depth check
/// is a cheap first filter, not a cryptographic guarantee; stake-binding and
/// retrievability auditing (storage incentives, tracked in #75) are the layer
/// that makes forgery unprofitable.
#[must_use]
pub fn required_receipt_depth(local_depth: NeighborhoodDepth, wire_radius: StorageRadius) -> u8 {
    let floor = local_depth.get().saturating_sub(SHALLOW_RECEIPT_TOLERANCE);
    floor.max(wire_radius.get())
}

/// Verify a recovered receipt signer is deep enough for the chunk.
///
/// Checks `PO(signer, chunk) >= required_receipt_depth(local_depth, wire_radius)`.
/// The signer overlay has already been recovered and verified at the pushsync
/// decode boundary, so this is a pure depth-policy comparison: a malformed
/// receipt never reaches here. On failure the typed [`ShallowReceipt`] carries
/// the required and observed depths for the caller's diagnostics and metrics.
pub fn verify_receipt_depth(
    signer: &OverlayAddress,
    address: &ChunkAddress,
    wire_radius: StorageRadius,
    local_depth: NeighborhoodDepth,
) -> Result<(), ShallowReceipt> {
    let required = required_receipt_depth(local_depth, wire_radius);
    let observed = address.proximity(signer).get();
    if observed < required {
        return Err(ShallowReceipt { required, observed });
    }
    Ok(())
}

#[cfg(test)]
mod receipt_depth_tests {
    use nectar_primitives::Bin;

    use super::*;

    fn depth(n: u8) -> NeighborhoodDepth {
        NeighborhoodDepth::new(Bin::new(n).unwrap())
    }

    fn radius(n: u8) -> StorageRadius {
        StorageRadius::new(Bin::new(n).unwrap())
    }

    /// A chunk address with a controlled leading byte, so a signer overlay's
    /// proximity to it is easy to reason about.
    fn chunk_address(first_byte: u8) -> ChunkAddress {
        let mut bytes = [0u8; 32];
        bytes[0] = first_byte;
        ChunkAddress::new(bytes)
    }

    /// An overlay sharing exactly `leading_bits` leading bits with `address`, so
    /// its proximity to the address is exactly `leading_bits`.
    fn overlay_at_proximity(address: &ChunkAddress, leading_bits: usize) -> OverlayAddress {
        let mut bytes = address.0.0;
        let byte = leading_bits / 8;
        let bit = 7 - (leading_bits % 8);
        if let Some(b) = bytes.get_mut(byte) {
            *b ^= 1 << bit;
        }
        OverlayAddress::from(bytes)
    }

    #[test]
    fn required_depth_floors_on_local_and_wire_radius_can_only_raise() {
        // The locally observed depth (minus the tolerance) is the floor; the
        // attacker-controlled wire radius can only RAISE the bar above it.
        // Floor dominates when the claimed radius is shallower.
        assert_eq!(
            required_receipt_depth(depth(8), radius(4)),
            8 - SHALLOW_RECEIPT_TOLERANCE
        );
        // A shallow (or zero) wire radius cannot drop the bar below the floor.
        assert_eq!(
            required_receipt_depth(depth(8), radius(0)),
            8 - SHALLOW_RECEIPT_TOLERANCE
        );
        // A deeper claimed radius raises the bar above the floor.
        assert_eq!(required_receipt_depth(depth(4), radius(8)), 8);
        // With local depth 0 the floor saturates to 0; only a positive claimed
        // radius can raise it (it never collapses below the radius claim).
        assert_eq!(required_receipt_depth(depth(0), radius(0)), 0);
        assert_eq!(required_receipt_depth(depth(0), radius(31)), 31);
    }

    #[test]
    fn wire_radius_zero_cannot_lower_the_bar_below_the_local_floor() {
        // Regression: an attacker setting storage_radius == 0 must NOT collapse
        // the required depth to 0. A signer far from the chunk (shallow) is
        // rejected even though it declares radius 0, because the locally observed
        // depth floors the requirement.
        let address = chunk_address(0xff);
        let signer = overlay_at_proximity(&address, 0);
        let observed = address.proximity(&signer).get();
        let local = observed + SHALLOW_RECEIPT_TOLERANCE + 1;
        let err = verify_receipt_depth(&signer, &address, radius(0), depth(local))
            .expect_err("radius 0 does not bypass the local floor");
        assert!(matches!(err, ShallowReceipt { .. }));
    }

    #[test]
    fn deep_enough_signer_is_accepted() {
        let address = chunk_address(0xff);
        let signer = overlay_at_proximity(&address, 8);
        verify_receipt_depth(&signer, &address, radius(8), depth(8)).expect("deep enough accepted");
    }

    #[test]
    fn shallow_signer_is_rejected() {
        let address = chunk_address(0xff);
        let signer = overlay_at_proximity(&address, 0);
        let observed = address.proximity(&signer).get();
        let local = observed + SHALLOW_RECEIPT_TOLERANCE + 1;
        let required = required_receipt_depth(depth(local), radius(0));
        let err = verify_receipt_depth(&signer, &address, radius(0), depth(local))
            .expect_err("shallow rejected");
        assert_eq!(err, ShallowReceipt { required, observed });
    }

    #[test]
    fn deeper_wire_radius_raises_the_bar_and_rejects_a_below_radius_signer() {
        // A storer that declares a deep radius binds itself to a stricter custody
        // claim. A signer below its own declared radius is rejected even when the
        // local floor alone would have accepted it.
        let address = chunk_address(0xff);
        let signer = overlay_at_proximity(&address, 4);
        let observed = address.proximity(&signer).get();
        assert!(
            observed < 12,
            "constructed signer sits below the claimed radius"
        );
        let err = verify_receipt_depth(&signer, &address, radius(12), depth(2))
            .expect_err("below-declared-radius signer rejected");
        assert_eq!(
            err,
            ShallowReceipt {
                required: 12,
                observed
            }
        );
    }
}
