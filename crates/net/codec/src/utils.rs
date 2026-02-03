//! Codec utility functions for common encoding patterns.

use alloy_primitives::U256;
use std::time::{SystemTime, UNIX_EPOCH};

/// Encode a U256 as big-endian bytes with leading zeros trimmed.
///
/// Matches Go's `big.Int.Bytes()` serialization used by Bee.
/// Returns an empty Vec for zero values.
#[inline]
pub fn encode_u256_be(value: U256) -> Vec<u8> {
    let bytes = value.to_be_bytes::<32>();
    match bytes.iter().position(|&b| b != 0) {
        Some(pos) => bytes[pos..].to_vec(),
        None => vec![],
    }
}

/// Decode big-endian bytes (with leading zeros trimmed) to U256.
///
/// Returns `U256::ZERO` for empty input.
#[inline]
pub fn decode_u256_be(bytes: &[u8]) -> U256 {
    if bytes.is_empty() {
        U256::ZERO
    } else {
        U256::from_be_slice(bytes)
    }
}

/// Returns the current Unix timestamp in seconds.
#[inline]
pub fn current_unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs()
}

/// Returns the current Unix timestamp in nanoseconds.
#[inline]
pub fn current_unix_timestamp_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_nanos() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_u256_zero() {
        let encoded = encode_u256_be(U256::ZERO);
        assert!(encoded.is_empty());
        assert_eq!(decode_u256_be(&encoded), U256::ZERO);
    }

    #[test]
    fn test_u256_small() {
        let value = U256::from(256u64);
        let encoded = encode_u256_be(value);
        assert_eq!(encoded, vec![0x01, 0x00]);
        assert_eq!(decode_u256_be(&encoded), value);
    }

    #[test]
    fn test_u256_large() {
        let value = U256::from(13_500_000u64);
        let encoded = encode_u256_be(value);
        assert_eq!(decode_u256_be(&encoded), value);
    }

    #[test]
    fn test_u256_max() {
        let value = U256::MAX;
        let encoded = encode_u256_be(value);
        assert_eq!(encoded.len(), 32);
        assert_eq!(decode_u256_be(&encoded), value);
    }

    #[test]
    fn test_timestamp_seconds() {
        let ts = current_unix_timestamp();
        assert!(ts > 1700000000); // After 2023
    }

    #[test]
    fn test_timestamp_nanos() {
        let ts = current_unix_timestamp_nanos();
        assert!(ts > 1_700_000_000_000_000_000); // After 2023
    }
}
