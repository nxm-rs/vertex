//! Witness selection for the proof of entitlement.
//!
//! The claim anchor deterministically picks which sample slots a prover must
//! open as witnesses, so a node cannot predict in advance which chunks it must
//! hold.

use crate::SAMPLE_SIZE;
use crate::anchor::ClaimAnchor;

/// The three sample slots a claim opens as witnesses, all at one BMT
/// [`segment_index`](Self::segment_index).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WitnessIndices {
    /// The two anchor-selected slots, in submission order: `claim mod
    /// (SAMPLE_SIZE - 1)`, then `claim mod (SAMPLE_SIZE - 2)` bumped past the
    /// first so the two stay distinct.
    pub challenged: [usize; 2],
    /// The final sample slot (`SAMPLE_SIZE - 1`); its maximal transformed
    /// address anchors the reserve-size estimate.
    pub last: usize,
    /// The BMT segment opened within each witnessed chunk: `claim mod 128`.
    pub segment_index: usize,
}

/// Derive the witness slots from the claim-time anchor, read as a big-endian
/// unsigned integer. These big-endian moduli are unrelated to the little-endian
/// `u64` BMT spans; do not conflate the two.
#[must_use]
pub fn witness_indices(claim: ClaimAnchor) -> WitnessIndices {
    let bytes = claim.as_bytes();
    let last = SAMPLE_SIZE - 1;
    let first = mod_be(bytes, last as u64) as usize;
    let mut second = mod_be(bytes, (last - 1) as u64) as usize;
    if second >= first {
        second += 1;
    }
    let segment_index = mod_be(bytes, 128) as usize;

    WitnessIndices {
        challenged: [first, second],
        last,
        segment_index,
    }
}

/// `big-endian(bytes) mod m`, computed without a big-integer dependency.
///
/// The `u128` accumulator never overflows: after each `% m` it is `< m <= u64`,
/// so shifting in one more byte stays within `u128`.
fn mod_be(bytes: &[u8], m: u64) -> u64 {
    let mut acc: u128 = 0;
    for &b in bytes {
        acc = ((acc << 8) | u128::from(b)) % u128::from(m);
    }
    acc as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;

    #[test]
    fn witness_indices_for_claim_value_30() {
        // claim anchor = 30 as a bytes32 (big-endian) -> [0, 3], 15, segment 30.
        let claim = ClaimAnchor::new(B256::left_padding_from(&[30]));
        let idx = witness_indices(claim);
        assert_eq!(idx.challenged, [0, 3]);
        assert_eq!(idx.last, 15);
        assert_eq!(idx.segment_index, 30);
    }

    #[test]
    fn mod_be_is_big_endian() {
        // 0x0100 = 256; 256 mod 15 = 1.
        assert_eq!(mod_be(&[0x01, 0x00], 15), 1);
        // 0x0001 = 1; 1 mod 15 = 1.
        assert_eq!(mod_be(&[0x00, 0x01], 15), 1);
        // 256 mod 128 = 0; 256 mod 14 = 4.
        assert_eq!(mod_be(&[0x01, 0x00], 128), 0);
        assert_eq!(mod_be(&[0x01, 0x00], 14), 4);
    }
}
