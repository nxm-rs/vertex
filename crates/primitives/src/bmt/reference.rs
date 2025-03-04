use alloy_primitives::keccak256;
use nectar_primitives_traits::SEGMENT_SIZE;
use rayon::prelude::*;
use std::cell::RefCell;
use thread_local::ThreadLocal;

pub struct RefHasher<const N: usize> {
    max_data_length: usize,
    segment_pair_length: usize,
    buffer_pool: ThreadLocal<RefCell<Vec<u8>>>,
}

impl<const N: usize> RefHasher<N> {
    pub fn new() -> Self {
        let mut c = 2;
        while c < N {
            c *= 2;
        }

        Self {
            segment_pair_length: 2 * SEGMENT_SIZE,
            max_data_length: c * SEGMENT_SIZE,
            buffer_pool: ThreadLocal::new(),
        }
    }

    #[inline(always)]
    pub fn hash(&self, data: &[u8]) -> [u8; 32] {
        let buffer = self
            .buffer_pool
            .get_or(|| RefCell::new(vec![0u8; self.max_data_length]));
        let mut buffer = buffer.borrow_mut();

        let len = data.len().min(self.max_data_length);
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), buffer.as_mut_ptr(), len);
            if len < self.max_data_length {
                buffer[len..].fill(0);
            }
        }

        self.hash_helper_parallel(&buffer, self.max_data_length)
    }

    #[inline(always)]
    fn hash_helper_parallel(&self, data: &[u8], length: usize) -> [u8; 32] {
        if length == self.segment_pair_length {
            return *keccak256(data);
        }

        let half = length / 2;
        let (left, right) = rayon::join(
            || self.hash_helper_parallel(&data[..half], half),
            || self.hash_helper_parallel(&data[half..], half),
        );

        let mut pair = [0u8; 2 * SEGMENT_SIZE];
        unsafe {
            std::ptr::copy_nonoverlapping(left.as_ptr(), pair.as_mut_ptr(), SEGMENT_SIZE);
            std::ptr::copy_nonoverlapping(
                right.as_ptr(),
                pair.as_mut_ptr().add(SEGMENT_SIZE),
                SEGMENT_SIZE,
            );
        }

        *keccak256(pair)
    }
}
