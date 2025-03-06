//! Reference implementation of a Binary Merkle Tree hasher.

use alloy_primitives::{keccak256, Keccak256, B256};
use digest::{Digest, FixedOutput, FixedOutputReset, OutputSizeUser, Reset, Update};
use generic_array::{typenum::U32, GenericArray};
use std::marker::PhantomData;

use super::constants::*;
use crate::chunk::ChunkAddress;
use crate::constants::HASH_SIZE;
use crate::error::Result;

/// Reference implementation of a BMT hasher that uses Keccak256
///
/// This implementation uses a fixed number of BMT branches (128) as defined by `BMT_BRANCHES`.
/// The Binary Merkle Tree is structured to efficiently hash data in parallel.
#[derive(Clone, Default)]
pub struct BMTHasher {
    span: u64,
    prefix: Vec<u8>,
    pending_data: Vec<u8>,
    _marker: PhantomData<Keccak256>,
}

impl BMTHasher {
    /// Create a new BMT hasher with `BMT_BRANCHES` (128) branches
    ///
    /// The hasher is optimized for data sized in multiples of SEGMENT_SIZE,
    /// with a maximum of BMT_BRANCHES * SEGMENT_SIZE bytes.
    pub fn new() -> Self {
        Self {
            span: 0,
            prefix: Vec::new(),
            pending_data: Vec::with_capacity(BMT_MAX_DATA_LENGTH),
            _marker: PhantomData,
        }
    }

    /// Set the span of data to be hashed
    pub fn set_span(&mut self, span: u64) {
        self.span = span;
    }

    /// Get the current span
    pub fn span(&self) -> u64 {
        self.span
    }

    /// Add a prefix to the hash calculation
    pub fn prefix_with(&mut self, prefix: &[u8]) {
        self.prefix = prefix.to_vec();
    }

    /// Compute the BMT hash and return the chunk address
    pub fn chunk_address(&mut self, data: &[u8]) -> Result<ChunkAddress> {
        let hash_bytes = <BMTHasher as Digest>::digest(data);
        ChunkAddress::from_slice(hash_bytes.as_slice()).map_err(|e| e.into())
    }

    /// Hash data using a binary merkle tree
    ///
    /// This function is optimized to efficiently hash data in parallel using
    /// a Binary Merkle Tree with `BMT_BRANCHES` (128) branches.
    #[inline(always)]
    fn hash_internal(&self, data: &[u8]) -> [u8; HASH_SIZE] {
        // Create a buffer for hashing
        let mut buffer = vec![0u8; BMT_MAX_DATA_LENGTH];

        // Copy data into buffer
        let len = data.len().min(BMT_MAX_DATA_LENGTH);
        buffer[..len].copy_from_slice(&data[..len]);

        // Process in parallel
        self.hash_helper_parallel(&buffer, BMT_MAX_DATA_LENGTH)
    }

    /// Recursively hash segments in parallel using rayon
    ///
    /// This is the core BMT hashing algorithm that divides work between threads
    /// for maximum parallelism.
    #[inline(always)]
    fn hash_helper_parallel(&self, data: &[u8], length: usize) -> [u8; HASH_SIZE] {
        if length == SEGMENT_PAIR_LENGTH {
            return *keccak256(data);
        }

        let half = length / 2;

        // Split data and hash both halves in parallel
        let (left, right) = data.split_at(half);
        let (left_hash, right_hash) = rayon::join(
            || self.hash_helper_parallel(left, half),
            || self.hash_helper_parallel(right, half),
        );

        // Combine the hashes
        let mut pair = [0u8; 2 * HASH_SIZE];
        pair[..HASH_SIZE].copy_from_slice(&left_hash);
        pair[HASH_SIZE..].copy_from_slice(&right_hash);

        *keccak256(&pair)
    }

    /// Finalize with span and optional prefix
    /// Returns a B256 directly
    #[inline(always)]
    fn finalize_with_prefix(&self, intermediate_hash: [u8; HASH_SIZE]) -> B256 {
        let mut hasher = Keccak256::new();

        // Add prefix if present
        if !self.prefix.is_empty() {
            hasher.update(&self.prefix);
        }

        // Add span as little-endian bytes
        hasher.update(self.span.to_le_bytes());

        // Add the intermediate hash
        hasher.update(intermediate_hash);

        // The keccak256 hasher returns a B256 directly
        hasher.finalize()
    }

    /// Get the hash as a B256 (preferred output format)
    pub fn hash_to_b256(&mut self, data: &[u8]) -> B256 {
        // Use trait implementation methods via fully qualified syntax
        <Self as Update>::update(self, data);
        let hash = self.hash_internal(&self.pending_data);
        let result = self.finalize_with_prefix(hash);
        <Self as Reset>::reset(self);
        result
    }
}

impl OutputSizeUser for BMTHasher {
    type OutputSize = U32; // 32-byte output size
}

impl Update for BMTHasher {
    fn update(&mut self, data: &[u8]) {
        self.pending_data.extend_from_slice(data);
    }
}

impl Reset for BMTHasher {
    fn reset(&mut self) {
        self.pending_data.clear();
        self.span = 0; // Reset span to 0
                       // Prefix is preserved intentionally
    }
}

impl FixedOutput for BMTHasher {
    fn finalize_into(self, out: &mut GenericArray<u8, Self::OutputSize>) {
        let hash = self.hash_internal(&self.pending_data);
        let final_hash = self.finalize_with_prefix(hash);
        out.copy_from_slice(final_hash.as_slice());
    }
}

impl FixedOutputReset for BMTHasher {
    fn finalize_into_reset(&mut self, out: &mut GenericArray<u8, Self::OutputSize>) {
        let hash = self.hash_internal(&self.pending_data);
        let final_hash = self.finalize_with_prefix(hash);
        out.copy_from_slice(final_hash.as_slice());
        // Call the Reset trait method using fully qualified syntax to avoid ambiguity
        <Self as Reset>::reset(self);
    }
}

// Make BMTHasher a valid hash function
impl digest::HashMarker for BMTHasher {}

/// A factory that creates BMTHasher instances
#[derive(Default, Clone)]
pub struct BMTHasherFactory;

impl BMTHasherFactory {
    /// Create a new factory for BMTHasher instances
    pub fn new() -> Self {
        Self
    }

    /// Create a new BMT hasher
    pub fn create_hasher(&self) -> BMTHasher {
        BMTHasher::new()
    }
}
