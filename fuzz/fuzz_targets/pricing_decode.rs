//! Fuzz the pricing wire decoder with raw attacker-controlled bytes.
//!
//! The generated protobuf struct is deserialized from the raw bytes, then
//! the domain conversion runs the canonical amount decode that guards the
//! node: the payment threshold is a peer-controlled monetary quantity, so a
//! decoded success must re-encode to the identical wire bytes, and an
//! oversized or leading-zero amount must error rather than truncate, wrap,
//! or panic. Any returned `Err` is success; the oracle is "no panic, no
//! OOM, no hang" plus the canonical re-encode assertion inside the entry
//! point.
//!
//! Seeds live in `fuzz/seeds/pricing_decode/` and are replayed on stable by
//! `seed_replay_pricing_decode` in `crates/swarm/net/pricing/src/codec.rs`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vertex_swarm_net_pricing::fuzz::decode_announce;

fuzz_target!(|data: &[u8]| {
    let _ = decode_announce(data);
});
