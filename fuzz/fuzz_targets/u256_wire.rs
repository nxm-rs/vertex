//! Dedicated property fuzz of the trimmed big-endian `U256` wire helpers.
//!
//! Every accounting codec funnels peer-controlled monetary quantities
//! through `decode_u256_be`/`encode_u256_be`, so the pair carries its own
//! target: decode must be the exact inverse of encode (an accepted input
//! re-encodes to the identical bytes, any 32-byte prefix taken as a value
//! round-trips), and oversized or leading-zero-abusive inputs must error
//! rather than truncate, wrap, or panic.
//!
//! Seeds live in `fuzz/seeds/u256_wire/` and are replayed on stable by
//! `seed_replay_u256_wire` in `crates/net/codec/src/utils.rs`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vertex_net_codec::fuzz::check_u256_wire;

fuzz_target!(|data: &[u8]| {
    check_u256_wire(data);
});
