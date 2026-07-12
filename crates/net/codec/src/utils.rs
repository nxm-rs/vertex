//! Codec utility functions for common encoding patterns.

use alloy_primitives::U256;

/// Malformed trimmed big-endian `U256` wire bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum U256DecodeError {
    /// More bytes than a 256-bit value can carry.
    #[error("u256 field too long: {0} bytes")]
    Oversized(usize),
    /// Non-canonical encoding: a leading zero byte.
    #[error("u256 field has a leading zero byte")]
    LeadingZero,
}

/// Encode a U256 as big-endian bytes with leading zeros trimmed.
///
/// Returns an empty Vec for zero values.
#[inline]
pub fn encode_u256_be(value: U256) -> Vec<u8> {
    let bytes = value.to_be_bytes::<32>();
    let start = bytes.iter().position(|&b| b != 0);
    match start.and_then(|pos| bytes.get(pos..)) {
        Some(trimmed) => trimmed.to_vec(),
        None => vec![],
    }
}

/// Decode trimmed big-endian bytes to U256; the exact inverse of
/// [`encode_u256_be`].
///
/// Empty input is zero. The decode is strict about canonical form: these are
/// peer-controlled monetary quantities, so anything [`encode_u256_be`] cannot
/// produce (a leading zero byte, more than 32 bytes) is an error, never a
/// trim, truncate, or panic.
#[inline]
pub fn decode_u256_be(bytes: &[u8]) -> Result<U256, U256DecodeError> {
    match bytes {
        [] => Ok(U256::ZERO),
        [0, ..] => Err(U256DecodeError::LeadingZero),
        _ => U256::try_from_be_slice(bytes).ok_or(U256DecodeError::Oversized(bytes.len())),
    }
}

/// Returns the current Unix timestamp in nanoseconds (0 if clock is before epoch).
#[inline]
pub fn current_unix_timestamp_nanos() -> i64 {
    vertex_util_runtime::time::now_unix_nanos()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_u256_zero() {
        let encoded = encode_u256_be(U256::ZERO);
        assert!(encoded.is_empty());
        assert_eq!(decode_u256_be(&encoded), Ok(U256::ZERO));
    }

    #[test]
    fn test_u256_small() {
        let value = U256::from(256u64);
        let encoded = encode_u256_be(value);
        assert_eq!(encoded, vec![0x01, 0x00]);
        assert_eq!(decode_u256_be(&encoded), Ok(value));
    }

    #[test]
    fn test_u256_large() {
        let value = U256::from(13_500_000u64);
        let encoded = encode_u256_be(value);
        assert_eq!(decode_u256_be(&encoded), Ok(value));
    }

    #[test]
    fn test_u256_max() {
        let value = U256::MAX;
        let encoded = encode_u256_be(value);
        assert_eq!(encoded.len(), 32);
        assert_eq!(decode_u256_be(&encoded), Ok(value));
    }

    #[test]
    fn test_u256_oversized_errors() {
        let bytes = [0xffu8; 33];
        assert_eq!(decode_u256_be(&bytes), Err(U256DecodeError::Oversized(33)));
    }

    #[test]
    fn test_u256_leading_zero_errors() {
        assert_eq!(
            decode_u256_be(&[0x00, 0x01]),
            Err(U256DecodeError::LeadingZero)
        );
        // Zero-padding past 32 bytes must not scan-and-truncate into a value
        // that happens to fit.
        let mut padded = vec![0x00u8];
        padded.extend_from_slice(&[0xff; 32]);
        assert_eq!(decode_u256_be(&padded), Err(U256DecodeError::LeadingZero));
    }

    #[test]
    fn test_timestamp_nanos() {
        let ts = current_unix_timestamp_nanos();
        assert!(ts > 1_700_000_000_000_000_000); // After 2023
    }

    /// Replay the committed fuzz seeds through the same invariant sweep the
    /// `u256_wire` fuzz target drives, so the stable test gate proves the
    /// seeds stay panic-free without the fuzzer.
    #[test]
    fn seed_replay_u256_wire() {
        let seed_dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../fuzz/seeds/u256_wire");
        let mut replayed = 0usize;
        for entry in std::fs::read_dir(&seed_dir)
            .unwrap_or_else(|e| panic!("seed dir {} must exist: {e}", seed_dir.display()))
        {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let data = std::fs::read(&path).unwrap();

            crate::fuzz::check_u256_wire(&data);
            if name.starts_with("valid-") {
                assert!(decode_u256_be(&data).is_ok(), "seed {name} must decode");
            } else if name.starts_with("invalid-") {
                assert!(
                    decode_u256_be(&data).is_err(),
                    "seed {name} must stay an Err"
                );
            } else {
                panic!("seed {name} matches no known prefix");
            }
            replayed += 1;
        }
        assert!(
            replayed >= 7,
            "expected at least the 7 curated seeds, found {replayed}"
        );
    }
}
