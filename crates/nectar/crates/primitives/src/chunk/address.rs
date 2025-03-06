//! Chunk address definition and operations

use crate::constants::*;
use crate::error::{ChunkError, Result};
use alloy_primitives::{hex, B256};

/// A 256 bit address for a chunk in the network
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChunkAddress(B256);

impl ChunkAddress {
    /// Creates a new ChunkAddress from raw bytes
    pub fn new(bytes: [u8; ADDRESS_SIZE]) -> Self {
        Self(B256::from_slice(&bytes))
    }

    /// Returns the underlying bytes
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }

    /// Creates a new address from a slice, checking the length
    pub fn from_slice(slice: &[u8]) -> Result<Self> {
        if slice.len() != ADDRESS_SIZE {
            return Err(ChunkError::size(
                "address must be exactly 32 bytes",
                slice.len(),
                ADDRESS_SIZE,
            )
            .into());
        }

        Ok(Self(B256::from_slice(slice)))
    }

    /// Checks if this address is zeros
    pub fn is_zero(&self) -> bool {
        self.0.is_zero()
    }

    /// Create a new zero-filled address
    pub fn zero() -> Self {
        Self(B256::ZERO)
    }

    /// Calculate proximity (0-256) between two addresses
    /// Returns the number of leading bits that match
    pub fn proximity(&self, other: &Self) -> u8 {
        // Count leading zeros in XOR distance
        let mut proximity = 0;
        let bytes1 = self.0.as_slice();
        let bytes2 = other.0.as_slice();

        for i in 0..ADDRESS_SIZE {
            let xor = bytes1[i] ^ bytes2[i];
            if xor == 0 {
                proximity += 8;
                continue;
            }
            // Count leading zeros in byte
            proximity += xor.leading_zeros() as u8;
            break;
        }
        proximity
    }

    /// Check if this address is within a certain proximity of another address
    pub fn is_within_proximity(&self, other: &Self, min_proximity: u8) -> bool {
        self.proximity(other) >= min_proximity
    }
}

impl Default for ChunkAddress {
    fn default() -> Self {
        Self(B256::ZERO)
    }
}

impl std::fmt::Display for ChunkAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", hex::encode(&self.0.as_slice()[..8]))
    }
}
