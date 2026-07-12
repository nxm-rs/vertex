//! Fuzz the embedded-client entry boundary: the helpers in `vertex-ffi` that
//! turn raw host bytes and strings into strong types. A panic there would
//! unwind across the cdylib into the host process, so the oracle is stricter
//! than no-crash: every malformed input maps to a typed `FfiError` and
//! valid-by-construction inputs are accepted.
//!
//! The shared driver sweeps chunk-address and stamp parsing, upload
//! reconstruction from raw payload plus stamp bytes (a really signed stamped
//! chunk must rebuild; a flipped stamp or address bit must not), identity key
//! handling (the 32-byte guard, then the scalar check), bootnode multiaddr
//! string parsing, the stream-config clamp, and the logging filter-directive
//! parse.
//!
//! Seeds live in `fuzz/seeds/ffi_entry/` and are replayed on stable by
//! `seed_replay_ffi_entry` in `crates/ffi/src/fuzz.rs`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vertex_ffi::fuzz::check_entry;

fuzz_target!(|data: &[u8]| {
    check_entry(data);
});
