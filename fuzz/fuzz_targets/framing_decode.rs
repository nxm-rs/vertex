//! Fuzz the uvarint length-delimited frame layer beneath every protocol
//! codec: the `Codec` wrapper and `FramedProto` in `vertex-net-codec`.
//!
//! The first input byte selects the feed chunk size and the rest is the raw
//! byte stream, decoded across a matrix of frame caps mirroring the
//! per-protocol budgets. The sweep pins the frame-layer contract: chunk
//! boundaries never change the decoded frames, truncated frames and
//! pathological varints error or wait but never panic, a declared length
//! above the cap is rejected on the bare prefix before any payload arrives,
//! garbage after a valid frame does not corrupt it, every yielded frame
//! survives decode-encode-decode, and the framed recv path agrees with the
//! raw decoder. Most of the frame arithmetic lives upstream in
//! `quick-protobuf-codec`; the target still pins the wrapper's error
//! mapping and the cap wiring, and catches regressions on dependency bumps.
//!
//! Seeds live in `fuzz/seeds/framing_decode/` and are replayed on stable by
//! `seed_replay_framing_decode` in `crates/net/codec/src/framed.rs`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vertex_net_codec::fuzz::check_framing;

fuzz_target!(|data: &[u8]| {
    check_framing(data);
});
