use alloy_primitives::keccak256;

use crate::HASH_SIZE;

use super::SEGMENT_PAIR_SIZE;

/// The non-optimised easy-to-read reference implementation of BMT
pub(crate) struct RefHasher {
    /// c * hashSize, where c = 2 ^ ceil(log2(count)), where count = ceil(length / hashSize)
    max_data_length: usize,
    /// 2 * hashSize
    segment_pair_length: usize,
}

impl RefHasher {
    /// Returns a new RefHasher
    pub(crate) fn new(count: usize) -> Self {
        let mut c = 2;

        while c < count {
            c *= 2;
        }

        Self {
            segment_pair_length: 2 * HASH_SIZE,
            max_data_length: c * HASH_SIZE,
        }
    }

    /// Returns the BMT hash of the byte slice
    pub(crate) fn hash(&self, data: &[u8]) -> [u8; 32] {
        // if data is shorter than base length (`max_data_length`), we provide padding with zeros.
        let mut d = vec![0u8; self.max_data_length];
        let len = data.len().min(self.max_data_length);
        d[..len].copy_from_slice(data);

        self.hash_helper(&d, self.max_data_length)
    }

    /// Calls itself recursively on both halves of the given slice, concatenating the results, and
    /// returns the hash of that.
    /// If the length of `data` is 2 * segment_size then just returns the hash of that segment
    /// pair.
    /// data has length max_data_length = segment size * 2 ^ k.
    fn hash_helper(&self, data: &[u8], mut length: usize) -> [u8; 32] {
        let mut pair = [0u8; SEGMENT_PAIR_SIZE];

        if length == self.segment_pair_length {
            pair.copy_from_slice(data);
        } else {
            // Data contains hashes of left and right BMT subtrees
            let half = length / 2;
            pair[..HASH_SIZE].copy_from_slice(&self.hash_helper(&data[..half], half));
            pair[HASH_SIZE..].copy_from_slice(&self.hash_helper(&data[half..], half));
        };
        *keccak256(pair)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloy_primitives::FixedBytes;
    use rand::Rng;

    type TestCase = (usize, usize, Box<dyn Fn(&[u8]) -> FixedBytes<32>>);

    #[test]
    fn test_reference() {
        let test_cases: Vec<TestCase> = vec![
            (
                1,
                2,
                Box::new(|d: &[u8]| {
                    let mut data = [0u8; 64];
                    data[..d.len()].copy_from_slice(d);
                    keccak256(data)
                }),
            ),
            (
                3,
                4,
                Box::new(|d: &[u8]| {
                    let mut data = [0u8; 128];
                    data[..d.len()].copy_from_slice(d);
                    keccak256([&keccak256(&data[..64]), &keccak256(&data[64..])].concat())
                }),
            ),
            (
                5,
                8,
                Box::new(|d: &[u8]| {
                    let mut data = [0u8; 256];
                    data[..d.len()].copy_from_slice(d);
                    keccak256(
                        [
                            &keccak256(
                                [&keccak256(&data[..64]), &keccak256(&data[64..128])].concat(),
                            ),
                            &keccak256(
                                [&keccak256(&data[128..192]), &keccak256(&data[192..])].concat(),
                            ),
                        ]
                        .concat(),
                    )
                }),
            ),
        ];

        for (from, to, expected_fn) in test_cases {
            for seg_count in from..=to {
                for length in 1..=(seg_count * 32) {
                    let mut data = vec![0u8; length];
                    rand::thread_rng().fill(&mut data[..]);

                    let expected = expected_fn(&data);

                    let hasher = RefHasher::new(seg_count);
                    let actual = hasher.hash(&data);

                    assert_eq!(
                        actual, expected,
                        "Failed for seg_count={}, length={}",
                        seg_count, length
                    );
                }
            }
        }
    }
}
