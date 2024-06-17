use crate::{distaddr::DistAddr, EXTENDED_PO, MAX_PO};

pub trait Proximity {
    fn proximity(&self, other: &DistAddr) -> u8;
    fn extended_proximity(&self, y: &DistAddr) -> u8;
}

impl Proximity for DistAddr {
    fn proximity(&self, other: &DistAddr) -> u8 {
        proximity_impl(self, other, MAX_PO)
    }

    fn extended_proximity(&self, other: &DistAddr) -> u8 {
        proximity_impl(self, other, EXTENDED_PO)
    }
}

// Proximity returns the proximity order of the MSB distance between x and y
//
// The distance metric MSB(x, y) of two equal length bit sequences x an y is the
// value of the binary integer cast of the x^y, ie., x and y bitwise xor-ed.
// the binary cast is big endian: most significant bit first (=MSB).
//
// Proximity(x, y) is a discrete logarithmic scaling of the MSB distance.
// It is defined as the reverse rank of the integer part of the base 2
// logarithm of the distance.
// It is calculated by counting the number of common leading zeros in the (MSB)
// binary representation of the x^y.
//
// (0 farthest, 255 closest, 256 self)
fn proximity_impl(one: &DistAddr, other: &DistAddr, max_po: u8) -> u8 {
    let b = (max_po / 8 + 1).min(one.len() as u8).min(other.len() as u8);
    for i in 0..b {
        let oxo = one[i as usize] ^ other[i as usize];
        for j in 0..8 {
            if (oxo >> (7 - j)) & 0x01 != 0 {
                return i * 8 + j;
            }
        }
    }
    max_po
}

#[cfg(test)]
mod tests {
    use alloy_primitives::FixedBytes;

    use crate::HASH_SIZE;

    use super::*;

    #[test]
    fn test_proximity() {
        let limit_po = |po: u8| -> u8 {
            if po > MAX_PO {
                MAX_PO
            } else {
                po
            }
        };

        let base: DistAddr = FixedBytes::from_slice(&[0; HASH_SIZE]);
        let test_cases = vec![
            (vec![0b00000000, 0b00000000, 0b00000000, 0b00000000], MAX_PO),
            (
                vec![0b10000000, 0b00000000, 0b00000000, 0b00000000],
                limit_po(0),
            ),
            (
                vec![0b01000000, 0b00000000, 0b00000000, 0b00000000],
                limit_po(1),
            ),
            (
                vec![0b00100000, 0b00000000, 0b00000000, 0b00000000],
                limit_po(2),
            ),
            (
                vec![0b00010000, 0b00000000, 0b00000000, 0b00000000],
                limit_po(3),
            ),
            (
                vec![0b00001000, 0b00000000, 0b00000000, 0b00000000],
                limit_po(4),
            ),
            (
                vec![0b00000100, 0b00000000, 0b00000000, 0b00000000],
                limit_po(5),
            ),
            (
                vec![0b00000010, 0b00000000, 0b00000000, 0b00000000],
                limit_po(6),
            ),
            (
                vec![0b00000001, 0b00000000, 0b00000000, 0b00000000],
                limit_po(7),
            ),
            (
                vec![0b00000000, 0b10000000, 0b00000000, 0b00000000],
                limit_po(8),
            ),
            (
                vec![0b00000000, 0b01000000, 0b00000000, 0b00000000],
                limit_po(9),
            ),
            (
                vec![0b00000000, 0b00100000, 0b00000000, 0b00000000],
                limit_po(10),
            ),
            (
                vec![0b00000000, 0b00010000, 0b00000000, 0b00000000],
                limit_po(11),
            ),
            (
                vec![0b00000000, 0b00001000, 0b00000000, 0b00000000],
                limit_po(12),
            ),
            (
                vec![0b00000000, 0b00000100, 0b00000000, 0b00000000],
                limit_po(13),
            ),
            (
                vec![0b00000000, 0b00000010, 0b00000000, 0b00000000],
                limit_po(14),
            ),
            (
                vec![0b00000000, 0b00000001, 0b00000000, 0b00000000],
                limit_po(15),
            ),
            (
                vec![0b00000000, 0b00000000, 0b10000000, 0b00000000],
                limit_po(16),
            ),
            (
                vec![0b00000000, 0b00000000, 0b01000000, 0b00000000],
                limit_po(17),
            ),
            (
                vec![0b00000000, 0b00000000, 0b00100000, 0b00000000],
                limit_po(18),
            ),
            (
                vec![0b00000000, 0b00000000, 0b00010000, 0b00000000],
                limit_po(19),
            ),
            (
                vec![0b00000000, 0b00000000, 0b00001000, 0b00000000],
                limit_po(20),
            ),
            (
                vec![0b00000000, 0b00000000, 0b00000100, 0b00000000],
                limit_po(21),
            ),
            (
                vec![0b00000000, 0b00000000, 0b00000010, 0b00000000],
                limit_po(22),
            ),
            (
                vec![0b00000000, 0b00000000, 0b00000001, 0b00000000],
                limit_po(23),
            ),
            (
                vec![0b00000000, 0b00000000, 0b00000000, 0b10000000],
                limit_po(24),
            ),
            (
                vec![0b00000000, 0b00000000, 0b00000000, 0b01000000],
                limit_po(25),
            ),
            (
                vec![0b00000000, 0b00000000, 0b00000000, 0b00100000],
                limit_po(26),
            ),
            (
                vec![0b00000000, 0b00000000, 0b00000000, 0b00010000],
                limit_po(27),
            ),
            (
                vec![0b00000000, 0b00000000, 0b00000000, 0b00001000],
                limit_po(28),
            ),
            (
                vec![0b00000000, 0b00000000, 0b00000000, 0b00000100],
                limit_po(29),
            ),
            (
                vec![0b00000000, 0b00000000, 0b00000000, 0b00000010],
                limit_po(30),
            ),
            (
                vec![0b00000000, 0b00000000, 0b00000000, 0b00000001],
                limit_po(31),
            ),
            (vec![], limit_po(31)),
            (vec![0b00000001], limit_po(7)),
            (vec![0b00000000], limit_po(31)),
            (
                vec![0b00000000, 0b00000000, 0b00000000, 0b00000000, 0b00000001],
                limit_po(31),
            ),
        ];

        for (addr, expected_po) in test_cases {
            let addr = DistAddr::right_padding_from(addr.as_slice());
            assert_eq!(
                base.proximity(&addr),
                expected_po,
                "base.proximity(&addr) failed"
            );
            assert_eq!(
                addr.proximity(&base),
                expected_po,
                "addr.proximity(&base) failed"
            );
        }
    }
}
