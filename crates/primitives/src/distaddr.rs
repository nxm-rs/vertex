use alloy_primitives::{FixedBytes, U256};

use crate::HASH_SIZE;

// A distance address represents an address in Swarm space in the domain of nodes, or chunks, whereby
// the consumer needs to be aware of their topological distance between distance addresses.
pub type DistAddr = FixedBytes<HASH_SIZE>;

pub trait Distance {
    fn length(&self, y: &DistAddr) -> U256;
    fn raw(&self, y: &DistAddr) -> FixedBytes<HASH_SIZE>;
    fn distance_cmp(&self, x: &DistAddr, y: &DistAddr) -> std::cmp::Ordering;
}

impl Distance for DistAddr {
    // Measure the length of the distance between addresses `x` and `y` in Swarm space.
    fn length(&self, y: &DistAddr) -> U256 {
        U256::from_be_slice(self.raw(y).as_ref())
    }

    // Determine the bytes distance between addresses `x` and `y`.
    fn raw(&self, y: &DistAddr) -> FixedBytes<HASH_SIZE> {
        FixedBytes::from_slice(
            self.0
                .iter()
                .zip(y.0.iter())
                .map(|(a, b)| a ^ b)
                .collect::<Vec<u8>>()
                .as_slice(),
        )
    }

    // Compares the distances of `x` and `y` to `self` in terms of the distance metric defined in Swarm space.
    fn distance_cmp(&self, x: &DistAddr, y: &DistAddr) -> std::cmp::Ordering {
        for i in 0..HASH_SIZE {
            let dx = x[i] ^ self[i];
            let dy = y[i] ^ self[i];
            if dx == dy {
                continue;
            } else if dx < dy {
                return std::cmp::Ordering::Greater;
            }
            return std::cmp::Ordering::Less;
        }

        std::cmp::Ordering::Equal
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    fn parse_hex_address(s: &str) -> DistAddr {
        s.parse().unwrap()
    }

    fn cmp_test_case(a: &DistAddr, x: &DistAddr, y: &DistAddr, expected: Ordering) {
        assert_eq!(a.distance_cmp(x, y), expected);
    }

    #[test]
    fn test_distance_raw() {
        let x =
            parse_hex_address("9100000000000000000000000000000000000000000000000000000000000000");
        let y =
            parse_hex_address("8200000000000000000000000000000000000000000000000000000000000000");
        let expected_result =
            "8593944123082061379093159043613555660984881674403010612303492563087302590464";

        let raw_distance = x.raw(&y);
        let raw_distance_u256 = U256::from_be_slice(&raw_distance.to_vec());

        assert_eq!(raw_distance_u256.to_string(), expected_result);
    }

    #[test]
    fn test_distance_cmp_greater() {
        let a =
            parse_hex_address("9100000000000000000000000000000000000000000000000000000000000000");
        let x =
            parse_hex_address("8200000000000000000000000000000000000000000000000000000000000000");
        let y =
            parse_hex_address("1200000000000000000000000000000000000000000000000000000000000000");

        cmp_test_case(&a, &x, &y, Ordering::Greater);
    }

    #[test]
    fn test_distance_cmp_less() {
        let a =
            parse_hex_address("9100000000000000000000000000000000000000000000000000000000000000");
        let x =
            parse_hex_address("1200000000000000000000000000000000000000000000000000000000000000");
        let y =
            parse_hex_address("8200000000000000000000000000000000000000000000000000000000000000");

        cmp_test_case(&a, &x, &y, Ordering::Less);
    }

    #[test]
    fn test_distance_cmp_equal() {
        let a =
            parse_hex_address("9100000000000000000000000000000000000000000000000000000000000000");
        let x =
            parse_hex_address("1200000000000000000000000000000000000000000000000000000000000000");
        let y =
            parse_hex_address("1200000000000000000000000000000000000000000000000000000000000000");

        cmp_test_case(&a, &x, &y, Ordering::Equal);
    }
}
