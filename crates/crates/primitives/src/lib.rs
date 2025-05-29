//! Core primitive types for the Vertex Swarm node
//!
//! This crate defines the basic types used throughout the Vertex Swarm project.

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::String, vec::Vec};

/// Error types for the Vertex Swarm node
pub mod error;
pub use error::*;

/// Chunk-related primitive types
pub mod chunk;
pub use chunk::*;

/// Network-related primitive types
pub mod network;
pub use network::*;

/// Utility functions
pub mod utils;

/// Result type used throughout the Vertex codebase
pub type Result<T> = core::result::Result<T, Error>;

/// A 32-byte address for chunks in the Swarm network
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ChunkAddress(pub [u8; 32]);

impl ChunkAddress {
    /// Creates a new ChunkAddress from raw bytes
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the underlying bytes
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Calculate proximity (0-256) between two addresses
    /// Returns the number of leading bits that match
    pub fn proximity(&self, other: &Self) -> u8 {
        // Count leading zeros in XOR distance
        let mut proximity = 0;
        for i in 0..32 {
            let xor = self.0[i] ^ other.0[i];
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

impl core::fmt::Debug for ChunkAddress {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "ChunkAddress({})", hex::encode(&self.0[..8]))
    }
}

impl core::fmt::Display for ChunkAddress {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", hex::encode(&self.0[..8]))
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for ChunkAddress {
    fn serialize<S>(&self, serializer: S) -> core::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&hex::encode(&self.0))
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for ChunkAddress {
    fn deserialize<D>(deserializer: D) -> core::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let bytes = hex::decode(s).map_err(serde::de::Error::custom)?;
        if bytes.len() != 32 {
            return Err(serde::de::Error::custom(
                "ChunkAddress must be exactly 32 bytes",
            ));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Ok(ChunkAddress(arr))
    }
}
