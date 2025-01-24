use alloy_primitives::keccak256;
use swarm_primitives_traits::SEGMENT_SIZE;

/// The non-optimised easy-to-read reference implementation of BMT
pub struct RefHasher<const N: usize> {
    /// c * hashSize, where c = 2 ^ ceil(log2(count)), where count = ceil(length / hashSize)
    max_data_length: usize,
    /// 2 * hashSize
    segment_pair_length: usize,
}

impl<const N: usize> Default for RefHasher<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> RefHasher<N> {
    /// Returns a new RefHasher
    pub fn new() -> Self {
        let mut c = 2;

        while c < N {
            c *= 2;
        }

        Self {
            segment_pair_length: 2 * SEGMENT_SIZE,
            max_data_length: c * SEGMENT_SIZE,
        }
    }

    /// Returns the BMT hash of the byte slice
    #[inline(always)]
    pub fn hash(&self, data: &[u8]) -> [u8; 32] {
        // if data is shorter than base length (`max_data_length`), we provide padding with zeros.
        let mut d = vec![0u8; self.max_data_length];
        let len = data.len().min(self.max_data_length);
        d[..len].copy_from_slice(&data[..len]);

        self.hash_helper(&d, self.max_data_length)
    }

    /// Calls itself recursively on both halves of the given slice, concatenating the results, and
    /// returns the hash of that.
    /// If the length of `data` is 2 * segment_size then just returns the hash of that segment
    /// pair.
    /// data has length max_data_length = segment size * 2 ^ k.
    #[inline(always)]
    fn hash_helper(&self, data: &[u8], length: usize) -> [u8; 32] {
        let mut pair = [0u8; (2 * SEGMENT_SIZE)];

        if length == self.segment_pair_length {
            pair.copy_from_slice(data);
        } else {
            // Data contains hashes of left and right BMT subtrees
            let half = length / 2;
            pair[..SEGMENT_SIZE].copy_from_slice(&self.hash_helper(&data[..half], half));
            pair[SEGMENT_SIZE..].copy_from_slice(&self.hash_helper(&data[half..], half));
        };
        *keccak256(pair)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloy_primitives::{b256, FixedBytes};
    use rand::Rng;

    #[test]
    fn test_simple() {
        let data: [u8; 3] = [1, 2, 3];

        let ref_bmt: RefHasher<128> = RefHasher::new();
        let ref_no_metahash = ref_bmt.hash(&data);
        let res_hash = *keccak256(
            [
                (data.len() as u64).to_le_bytes().as_slice(),
                ref_no_metahash.as_slice(),
            ]
            .concat(),
        );
        assert_eq!(
            res_hash,
            b256!("ca6357a08e317d15ec560fef34e4c45f8f19f01c372aa70f1da72bfa7f1a4338")
        );
    }

    /// Macro to generate a test case for a specific buffer size `N`
    macro_rules! test_ref_hasher {
        ($name:ident, $N:expr, $expected_fn:expr) => {
            #[test]
            fn $name() {
                for length in 1..=$N {
                    let mut data = vec![0u8; length];
                    rand::thread_rng().fill(&mut data[..]);

                    let expected = $expected_fn(&data);
                    let hasher = RefHasher::<$N>::new();
                    let actual = hasher.hash(&data);

                    assert_eq!(actual, expected, "Failed for N={}, length={}", $N, length);
                }
            }
        };
    }

    fn expected_fn_2(d: &[u8]) -> FixedBytes<32> {
        let mut data = [0u8; 2 * SEGMENT_SIZE];
        data[..d.len()].copy_from_slice(d);
        keccak256(data)
    }

    fn expected_fn_4(d: &[u8]) -> FixedBytes<32> {
        let mut data = [0u8; 4 * SEGMENT_SIZE];
        data[..d.len()].copy_from_slice(d);
        keccak256([&keccak256(&data[..64]), &keccak256(&data[64..])].concat())
    }

    fn expected_fn_8(d: &[u8]) -> FixedBytes<32> {
        let mut data = [0u8; 8 * SEGMENT_SIZE];
        data[..d.len()].copy_from_slice(d);
        keccak256(
            [
                &keccak256([&keccak256(&data[..64]), &keccak256(&data[64..128])].concat()),
                &keccak256([&keccak256(&data[128..192]), &keccak256(&data[192..])].concat()),
            ]
            .concat(),
        )
    }

    // Generate tests for different buffer sizes
    test_ref_hasher!(test_ref_hasher_2_segments, 2, expected_fn_2);
    test_ref_hasher!(test_ref_hasher_4_segments, 4, expected_fn_4);
    test_ref_hasher!(test_ref_hasher_8_segments, 8, expected_fn_8);
}
