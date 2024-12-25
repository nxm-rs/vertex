use std::cmp::Ordering;

use alloy_primitives::U256;

use crate::Address;

pub trait Distance {
    /// Returns true if self is closer to `a` than to `y`
    fn closer(&self, a: &Self, y: &Self) -> bool;
}

impl Distance for Address {
    fn closer(&self, a: &Self, y: &Self) -> bool {
        // TODO: why is the Equal case not counted, and how much is this closer function used?
        matches!(distance_cmp(a, self, y), Ordering::Less)
    }
}

/// Retuns the distance between address `x` and address `y` in big-endian.
/// Does not check the length as `Address` is a fixed length.
pub fn distance(x: &Address, y: &Address) -> U256 {
    let result: Vec<u8> = x.0.iter().zip(y.0.iter()).map(|(a, b)| a ^ b).collect();

    U256::from_be_slice(&result)
}

/// Compares `x` and `y` to `a` in terms of the distance metric defined in the Swarm specification:
/// It returns:
///   - `Ordering::Greater` if `x` is closer to `a` than `y`
///   - `Ordering::Equal` if `x` and `y` are equidistant from `a` (this means that `x` and `y`
///     are the same address)
///   - `Ordering::Less` if `x` is farther from `a` than `y`
///
/// NB: Unlike `bee`, there is no fail condition with the length being different. The `Address`
///     type is explicit about its length.
pub fn distance_cmp(a: &Address, x: &Address, y: &Address) -> std::cmp::Ordering {
    let (ab, xb, yb) = (&a.0, &x.0, &y.0);

    for i in 0..ab.len() {
        let dx = xb[i] ^ ab[i];
        let dy = yb[i] ^ ab[i];

        if dx != dy {
            return match dx < dy {
                true => Ordering::Greater,
                false => Ordering::Less,
            };
        }
    }

    Ordering::Equal
}

#[cfg(test)]
mod tests {
    use alloy_primitives::b256;
    use std::{cmp::Ordering, str::FromStr};

    use super::*;

    #[test]
    fn distance_closer() {
        let a: Address = b256!("9100000000000000000000000000000000000000000000000000000000000000");
        let x: Address = b256!("8200000000000000000000000000000000000000000000000000000000000000");
        let y: Address = b256!("1200000000000000000000000000000000000000000000000000000000000000");

        assert!(!x.closer(&a, &y));
    }

    #[test]
    fn distance_matches() {
        let x: Address = b256!("9100000000000000000000000000000000000000000000000000000000000000");
        let y: Address = b256!("8200000000000000000000000000000000000000000000000000000000000000");

        assert_eq!(
            distance(&x, &y),
            U256::from_str(
                "8593944123082061379093159043613555660984881674403010612303492563087302590464"
            )
            .unwrap()
        );
    }

    macro_rules! distance_cmp_test {
        ($test_name:ident, $ordering:expr, $a:expr, $x:expr, $y:expr) => {
            #[test]
            fn $test_name() {
                assert_eq!(distance_cmp(&b256!($a), &b256!($x), &b256!($y)), $ordering);
            }
        };
    }

    distance_cmp_test!(
        distance_cmp_eq,
        Ordering::Equal,
        "9100000000000000000000000000000000000000000000000000000000000000",
        "1200000000000000000000000000000000000000000000000000000000000000",
        "1200000000000000000000000000000000000000000000000000000000000000"
    );

    distance_cmp_test!(
        distance_cmp_lt,
        Ordering::Less,
        "9100000000000000000000000000000000000000000000000000000000000000",
        "1200000000000000000000000000000000000000000000000000000000000000",
        "8200000000000000000000000000000000000000000000000000000000000000"
    );

    distance_cmp_test!(
        distance_cmp_gt,
        Ordering::Greater,
        "9100000000000000000000000000000000000000000000000000000000000000",
        "8200000000000000000000000000000000000000000000000000000000000000",
        "1200000000000000000000000000000000000000000000000000000000000000"
    );
}
