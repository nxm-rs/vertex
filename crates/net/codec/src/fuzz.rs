//! Fuzz-facing surface behind the `arbitrary` feature: the invariant sweep
//! for the trimmed big-endian `U256` wire helpers, shared between the
//! `u256_wire` fuzz target and the stable seed-replay test. Dev-only; never
//! enabled by a shipped artefact cone.

use alloy_primitives::U256;

use crate::utils::{U256DecodeError, decode_u256_be, encode_u256_be};

/// Run the `u256_wire` invariant sweep over one raw input.
///
/// Decode must be the exact inverse of encode: an accepted input re-encodes
/// to the identical bytes, a rejected input is provably non-canonical
/// (oversized or leading-zero), and any 32-byte prefix taken as a value
/// round-trips through encode then decode. A violation panics.
pub fn check_u256_wire(data: &[u8]) {
    match decode_u256_be(data) {
        Ok(value) => assert_eq!(
            encode_u256_be(value).as_slice(),
            data,
            "an accepted input must re-encode to the identical bytes"
        ),
        Err(U256DecodeError::Oversized(len)) => {
            assert_eq!(len, data.len());
            assert!(len > 32, "oversized rejection requires more than 32 bytes");
        }
        Err(U256DecodeError::LeadingZero) => assert_eq!(
            data.first(),
            Some(&0),
            "leading-zero rejection requires a leading zero byte"
        ),
    }

    if let Some(prefix) = data.first_chunk::<32>() {
        let value = U256::from_be_bytes(*prefix);
        assert_eq!(
            decode_u256_be(&encode_u256_be(value)),
            Ok(value),
            "decode(encode(x)) must reproduce the value"
        );
    }
}
