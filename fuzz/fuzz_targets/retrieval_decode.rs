//! Fuzz the retrieval wire decoders with raw attacker-controlled bytes.
//!
//! Each input is parsed as both retrieval frames: the generated protobuf
//! struct is deserialized from the raw bytes, then the domain conversion
//! runs the validation that guards the node. The request arm checks the
//! 32-byte address length; the delivery arm takes the input's first 32
//! bytes as the requested address and the rest as the raw frame, so the
//! fuzzer controls both sides of the chunk-delivery validation: the
//! empty-data failure sentinel, the optional stamp parse, and the chunk
//! reconstruction against the requested address. A decoded success must
//! carry the requested address. Any returned `Err` is success; the oracle
//! is "no panic, no OOM, no hang".
//!
//! Seeds live in `fuzz/seeds/retrieval_decode/` and are replayed on stable
//! by `seed_replay_retrieval_decode` in
//! `crates/swarm/net/retrieval/src/codec.rs`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vertex_swarm_net_retrieval::fuzz::{decode_delivery, decode_request};

fuzz_target!(|data: &[u8]| {
    let _ = decode_request(data);
    let _ = decode_delivery(data);
});
