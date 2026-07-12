//! Fuzz the pseudosettle wire decoders with raw attacker-controlled bytes.
//!
//! Each input is parsed as both pseudosettle frames: the generated protobuf
//! struct is deserialized from the raw bytes, then the domain conversion
//! runs the canonical amount decode that guards the node. The amounts are
//! peer-controlled monetary quantities, so a decoded success must re-encode
//! to the identical wire bytes, and an oversized or leading-zero amount
//! must error rather than truncate, wrap, or panic. Any returned `Err` is
//! success; the oracle is "no panic, no OOM, no hang" plus the canonical
//! re-encode assertion inside the entry points.
//!
//! Seeds live in `fuzz/seeds/pseudosettle_decode/` and are replayed on
//! stable by `seed_replay_pseudosettle_decode` in
//! `crates/swarm/net/pseudosettle/src/codec.rs`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vertex_swarm_net_pseudosettle::fuzz::{decode_payment, decode_payment_ack};

fuzz_target!(|data: &[u8]| {
    let _ = decode_payment(data);
    let _ = decode_payment_ack(data);
});
