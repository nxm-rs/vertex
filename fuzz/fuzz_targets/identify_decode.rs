//! Fuzz the identify decode path: the message conversions over the vendored
//! generated struct (public-key decode, multiaddr and protocol filtering, and
//! the signed-peer-record consistency gate).
//!
//! The generated message is flat, so raw attacker bytes go straight through
//! the generated reader; a structured arm builds messages with really
//! decodable public keys and really signed peer records (matching or
//! mismatching the key) so the validated paths stay reachable. The push
//! conversion is total; the identify conversion may reject. The oracle is
//! "no panic, no OOM, no hang".
//!
//! Seeds live in `fuzz/seeds/identify_decode/` and are replayed on stable by
//! `seed_replay_identify_decode` in
//! `crates/swarm/net/identify/src/protocol.rs`.

#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;
use vertex_swarm_net_identify::fuzz::{
    MAX_MESSAGE_SIZE_BYTES, arbitrary_identify_bytes, decode_identify,
};

fuzz_target!(|data: &[u8]| {
    // Raw arm, capped at the frame limit the production codec enforces.
    let capped = &data[..data.len().min(MAX_MESSAGE_SIZE_BYTES)];
    if let Some((_info, push)) = decode_identify(capped) {
        assert!(push.is_ok(), "the push conversion is total");
    }

    // Structured arm: writer-produced bytes with adversarial field content.
    let mut u = Unstructured::new(data);
    if let Ok(bytes) = arbitrary_identify_bytes(&mut u) {
        if let Some((_info, push)) = decode_identify(&bytes) {
            assert!(push.is_ok(), "the push conversion is total");
        }
    }
});
