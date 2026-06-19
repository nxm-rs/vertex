//! Witness selection for the proof of entitlement.
//!
//! At claim time the prover must open a few sample slots as witnesses. The
//! claim anchor deterministically picks which ones, so a node cannot predict in
//! advance which chunks it must hold.

use crate::SAMPLE_SIZE;
use crate::anchor::ClaimAnchor;

/// The sample slots a claim opens as witnesses.
///
/// In the redistribution game the prover opens three slots of the reserve
/// sample: the two [`challenged`](Self::challenged) slots the claim anchor
/// pseudo-randomly selects (so the prover cannot precompute which chunks to
/// fake), and the [`last`](Self::last) slot, whose maximal transformed address
/// the game uses to estimate the reserve size. All three are opened at the same
/// BMT [`segment_index`](Self::segment_index).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WitnessIndices {
    /// The two slots the claim anchor pseudo-randomly selects to challenge, in
    /// submission order: `claim mod (SAMPLE_SIZE - 1)`, and a second draw
    /// `claim mod (SAMPLE_SIZE - 2)` bumped past the first so the two stay
    /// distinct.
    pub challenged: [usize; 2],
    /// The final sample slot (`SAMPLE_SIZE - 1`), always opened; its maximal
    /// transformed address anchors the reserve-size estimate.
    pub last: usize,
    /// The BMT segment opened within each witnessed chunk: `claim mod 128`.
    pub segment_index: usize,
}

/// Derive the witness slots from the claim-time anchor.
///
/// The claim anchor is interpreted as a **big-endian** unsigned integer:
///
/// - `challenged[0] = claim mod 15`
/// - `challenged[1] = claim mod 14`, incremented by one if `>= challenged[0]` so
///   the two challenged slots are distinct.
/// - `last = 15` (the final sample slot).
/// - `segment_index = claim mod 128`.
///
/// These big-endian moduli are unrelated to the little-endian `u64` BMT spans;
/// the two must not be conflated.
#[must_use]
pub fn witness_indices(claim: ClaimAnchor) -> WitnessIndices {
    let bytes = claim.as_bytes();
    let last = SAMPLE_SIZE - 1; // 15
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
