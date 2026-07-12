//! Fuzz the swap wire decoders with raw attacker-controlled bytes.
//!
//! Each input is parsed as both swap frames: the generated protobuf struct
//! is deserialized from the raw bytes, then the domain conversion runs the
//! validation that guards the node. The cheque arm drives the JSON payload
//! path (addresses, the bare-decimal payout across the full 256-bit range,
//! the fixed 65-byte base64 signature) and asserts a decoded cheque stays
//! stable across re-encode; the handshake arm checks the 20-byte
//! beneficiary length. Any returned `Err` is success; the oracle is "no
//! panic, no OOM, no hang" plus the re-encode assertion inside the cheque
//! entry point.
//!
//! Seeds live in `fuzz/seeds/swap_decode/` and are replayed on stable by
//! `seed_replay_swap_decode` in `crates/swarm/net/swap/src/codec.rs`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vertex_swarm_net_swap::fuzz::{decode_emit_cheque, decode_handshake};

fuzz_target!(|data: &[u8]| {
    let _ = decode_emit_cheque(data);
    let _ = decode_handshake(data);
});
