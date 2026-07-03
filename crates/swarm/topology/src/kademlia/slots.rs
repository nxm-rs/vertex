//! Sub-prefix slot math for balanced-bin dial diversity.
//!
//! A local dial policy, not wire-visible: it never changes bytes on the wire,
//! only which known peers a node chooses to dial. A bin's address space is
//! partitioned into `2^BIT_SUFFIX_LENGTH` slots keyed by the suffix bits
//! immediately after the bin's differing bit, so a bin's connected set can be
//! spread across its sub-tries instead of clustering in one.

use vertex_swarm_primitives::{Bin, OverlayAddress};

/// Suffix-bit width of a slot key: `2^4 = 16` slots per bin.
pub(crate) const BIT_SUFFIX_LENGTH: u8 = 4;

/// The slot an overlay occupies within `bin`.
///
/// Reads the `BIT_SUFFIX_LENGTH` bits at positions `bin+1 ..= bin+BIT_SUFFIX_LENGTH`
/// (MSB-first) from the overlay. Two peers share a slot iff those bits agree.
/// The top bit position is at most `Bin::MAX + BIT_SUFFIX_LENGTH`, well inside
/// the 256-bit address, so a missing byte is treated as zero rather than
/// indexed.
pub(crate) fn slot_of(overlay: &OverlayAddress, bin: Bin) -> u8 {
    let bytes = overlay.as_bytes();
    let mut slot = 0u8;
    for i in 0..BIT_SUFFIX_LENGTH {
        let pos = bin.get() as usize + 1 + i as usize;
        let bit = bytes
            .get(pos / 8)
            .map_or(0, |byte| (byte >> (7 - (pos % 8))) & 1);
        slot = (slot << 1) | bit;
    }
    slot
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use super::*;

    fn b(n: u8) -> Bin {
        Bin::new(n).expect("valid bin")
    }

    /// Build a 32-byte overlay from an explicit bit list at MSB-first positions.
    fn overlay_with_bits(bits: &[(usize, u8)]) -> OverlayAddress {
        let mut bytes = [0u8; 32];
        for &(pos, bit) in bits {
            if bit == 1 {
                bytes[pos / 8] |= 0x80 >> (pos % 8);
            }
        }
        OverlayAddress::from(bytes)
    }

    #[test]
    fn slot_reads_the_four_bits_after_the_differing_bit() {
        // Bin 0: slot bits are positions 1..=4. Set them to 1011 = 11.
        let overlay = overlay_with_bits(&[(1, 1), (2, 0), (3, 1), (4, 1)]);
        assert_eq!(slot_of(&overlay, b(0)), 0b1011);
    }

    #[test]
    fn slot_spans_a_byte_boundary_at_bin_6() {
        // Bin 6: slot bits are positions 7,8,9,10 - position 7 in byte 0, the
        // rest in byte 1. Set 1101 across the boundary.
        let overlay = overlay_with_bits(&[(7, 1), (8, 1), (9, 0), (10, 1)]);
        assert_eq!(slot_of(&overlay, b(6)), 0b1101);
    }

    #[test]
    fn slot_spans_a_byte_boundary_at_bin_7() {
        // Bin 7: slot bits are positions 8,9,10,11, entirely in byte 1.
        let overlay = overlay_with_bits(&[(8, 1), (9, 0), (10, 1), (11, 0)]);
        assert_eq!(slot_of(&overlay, b(7)), 0b1010);
    }

    #[test]
    fn slot_ignores_the_deep_uniqueness_byte() {
        // Byte 31 disambiguates overlays without moving the slot: two overlays
        // agreeing on the slot bits share a slot regardless of byte 31.
        let mut a = [0u8; 32];
        a[0] = 0b0101_0000;
        let mut c = a;
        a[31] = 0x11;
        c[31] = 0xEE;
        let oa = OverlayAddress::from(a);
        let oc = OverlayAddress::from(c);
        assert_eq!(slot_of(&oa, b(0)), slot_of(&oc, b(0)));
        assert_eq!(slot_of(&oa, b(0)), 0b1010);
    }

    #[test]
    fn slot_is_deterministic() {
        let overlay = overlay_with_bits(&[(2, 1), (3, 1)]);
        assert_eq!(slot_of(&overlay, b(1)), slot_of(&overlay, b(1)));
    }
}
