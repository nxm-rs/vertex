//! Fuzz the pullsync selection bitvector, the hand-rolled bit-packed set
//! decoder behind the `Want` frame.
//!
//! The whole check lives in `vertex_swarm_net_pullsync::fuzz::check_bitvector`
//! so this target and the stable seed-replay test drive one path: the input
//! runs through `from_wire_bytes` (the `Want` decode, byte length as the only
//! length signal), then a two-byte length prefix plus the remaining bytes run
//! through `BitVector::from_bytes`. Asserted invariants: the `len / 8 + 1`
//! byte-length rule, trailing pad bits never surfacing through
//! `get`/`count_ones`, `set` past `len` staying a no-op, and an
//! `into_bytes`/`from_bytes` round-trip reproducing the vector.
//!
//! Seeds live in `fuzz/seeds/pullsync_bitvector/` and are replayed on stable
//! by `seed_replay_pullsync_bitvector` in
//! `crates/swarm/net/pullsync/src/bitvector.rs`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vertex_swarm_net_pullsync::fuzz::check_bitvector;

fuzz_target!(|data: &[u8]| {
    let _ = check_bitvector(data);
});
