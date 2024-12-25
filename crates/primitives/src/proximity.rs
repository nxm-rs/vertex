use crate::{EXTENDED_PO, MAX_PO};

/// Proximity returns the proximity order of the MSB distance between `x` and `y`
///
/// The distance metric MSB(x, y) of two equal length byte sequences `x` and `y`
/// is the value of the binary integer cast of the x^y, ie., `x` and `y` bitwise
/// xor-ed. The binary cast is big endeian: most significant bit first (MSB).
///
/// Proximity(x, y) is a discrete logarithmic scaling of the MSB distance.
/// It is defined as the reverse rank of the integer part of the base 2 logarithm
/// of the distance.
///
/// It is calculated by counting the number of common leading zeros in the (MSB)
/// binary representation of the x ^ y.
#[inline]
fn proximity_helper(x: &[u8], y: &[u8], max: usize) -> u8 {
    x.iter()
        .zip(y.iter())
        .take((max / 8 + 1) as usize)
        .enumerate()
        .find_map(|(i, (&o1, &o2))| {
            let oxo = o1 ^ o2;
            (0..8)
                .find(|&j| (oxo >> (7 - j)) & 0x01 != 0)
                .map(|pos| i as u8 * 8 + pos)
        })
        .unwrap_or(max.try_into().unwrap())
}

pub fn proximity(x: &[u8], y: &[u8]) -> u8 {
    proximity_helper(x, y, MAX_PO)
}

pub fn extended_proximity(x: &[u8], y: &[u8]) -> u8 {
    proximity_helper(x, y, EXTENDED_PO)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MAX_PO;

    // Function to limit the proximity to MAX_PO
    const fn limit_po(po: usize) -> usize {
        if po > MAX_PO {
            MAX_PO
        } else {
            po
        }
    }

    /// Table-driven test case structure
    struct TestCase {
        addr: &'static [u8],
        expected_po: usize,
    }

    /// Macro for generating the test cases
    macro_rules! proximity_test_cases {
        ($($addr:expr => $po:expr),* $(,)?) => {
            &[
                $(
                    TestCase {
                        addr: &$addr,
                        expected_po: limit_po($po),
                    }
                ),*
            ]
        };
    }

    #[test]
    fn test_proximity() {
        let base: &[u8] = &[0b00000000, 0b00000000, 0b00000000, 0b00000000];

        let test_cases = proximity_test_cases!(
            [0b00000000, 0b00000000, 0b00000000, 0b00000000] => MAX_PO,
            [0b10000000, 0b00000000, 0b00000000, 0b00000000] => 0,
            [0b01000000, 0b00000000, 0b00000000, 0b00000000] => 1,
            [0b00100000, 0b00000000, 0b00000000, 0b00000000] => 2,
            [0b00010000, 0b00000000, 0b00000000, 0b00000000] => 3,
            [0b00001000, 0b00000000, 0b00000000, 0b00000000] => 4,
            [0b00000100, 0b00000000, 0b00000000, 0b00000000] => 5,
            [0b00000010, 0b00000000, 0b00000000, 0b00000000] => 6,
            [0b00000001, 0b00000000, 0b00000000, 0b00000000] => 7,
            [0b00000000, 0b10000000, 0b00000000, 0b00000000] => 8,
            [0b00000000, 0b01000000, 0b00000000, 0b00000000] => 9,
            [0b00000000, 0b00100000, 0b00000000, 0b00000000] => 10,
            [0b00000000, 0b00010000, 0b00000000, 0b00000000] => 11,
            [0b00000000, 0b00001000, 0b00000000, 0b00000000] => 12,
            [0b00000000, 0b00000100, 0b00000000, 0b00000000] => 13,
            [0b00000000, 0b00000010, 0b00000000, 0b00000000] => 14,
            [0b00000000, 0b00000001, 0b00000000, 0b00000000] => 15,
            [0b00000000, 0b00000000, 0b10000000, 0b00000000] => 16,
            [0b00000000, 0b00000000, 0b01000000, 0b00000000] => 17,
            [0b00000000, 0b00000000, 0b00100000, 0b00000000] => 18,
            [0b00000000, 0b00000000, 0b00010000, 0b00000000] => 19,
            [0b00000000, 0b00000000, 0b00001000, 0b00000000] => 20,
            [0b00000000, 0b00000000, 0b00000100, 0b00000000] => 21,
            [0b00000000, 0b00000000, 0b00000010, 0b00000000] => 22,
            [0b00000000, 0b00000000, 0b00000001, 0b00000000] => 23,
            [0b00000000, 0b00000000, 0b00000000, 0b10000000] => 24,
            [0b00000000, 0b00000000, 0b00000000, 0b01000000] => 25,
            [0b00000000, 0b00000000, 0b00000000, 0b00100000] => 26,
            [0b00000000, 0b00000000, 0b00000000, 0b00010000] => 27,
            [0b00000000, 0b00000000, 0b00000000, 0b00001000] => 28,
            [0b00000000, 0b00000000, 0b00000000, 0b00000100] => 29,
            [0b00000000, 0b00000000, 0b00000000, 0b00000010] => 30,
            [0b00000000, 0b00000000, 0b00000000, 0b00000001] => 31,
            [] => 31,
            [0b00000001] => 7,
            [0b00000000] => 31,
            [0b00000000, 0b00000000, 0b00000000, 0b00000000, 0b00000001] => 31,
        );

        for tc in test_cases {
            let got = proximity(base, tc.addr) as usize;
            assert_eq!(
                got, tc.expected_po,
                "Test failed for addr: {:?}, got {}, expected {}",
                tc.addr, got, tc.expected_po
            );

            let got_reverse = proximity(tc.addr, base) as usize;
            assert_eq!(
                got_reverse, tc.expected_po,
                "Test failed for reversed addr: {:?}, got {}, expected {}",
                tc.addr, got_reverse, tc.expected_po
            );
        }
    }
}
