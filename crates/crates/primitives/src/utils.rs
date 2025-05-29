//! Utility functions for Vertex Swarm

use crate::ChunkAddress;
use sha3::{Digest, Keccak256};

#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

/// Computes the Keccak256 hash of data
pub fn keccak256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Computes a chunk address from data using Keccak256
pub fn chunk_address_from_data(data: &[u8]) -> ChunkAddress {
    ChunkAddress::new(keccak256(data))
}

/// Compute proximity order between two byte arrays (0-256)
pub fn proximity_order(a: &[u8], b: &[u8]) -> u8 {
    debug_assert_eq!(a.len(), b.len(), "Arrays must be of equal length");

    let mut proximity = 0;
    for i in 0..a.len() {
        let xor = a[i] ^ b[i];
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

/// Bytes to hex string
pub fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        s.push_str(&format!("{:02x}", byte));
    }
    s
}

/// Hex string to bytes
pub fn hex_to_bytes(hex: &str) -> Result<Vec<u8>, hex::FromHexError> {
    // Strip 0x prefix if present
    let hex = hex.strip_prefix("0x").unwrap_or(hex);
    hex::decode(hex)
}

/// Converts a slice of bytes to a fixed-size array
pub fn to_array<const N: usize>(slice: &[u8]) -> Option<[u8; N]> {
    if slice.len() != N {
        return None;
    }

    let mut arr = [0u8; N];
    arr.copy_from_slice(slice);
    Some(arr)
}
