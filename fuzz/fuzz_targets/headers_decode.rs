//! Fuzz the stream-headers envelope decode, the exchange every headered
//! protocol negotiation runs before any protocol logic, so malformed-header
//! handling is on the path of every connection.
//!
//! The flat `Header` entry is fed raw attacker bytes straight through the
//! generated reader (which rejects a non-UTF-8 key); the nested `Headers`
//! envelope goes through the real write then read path with adversarial
//! entries (arbitrary keys and values, deliberately repeated keys), so the
//! domain conversion sees hostile input while the reader stays on
//! writer-produced bytes. The envelope is not raw-fed pending the reported
//! nested-length soundness issue in the upstream `quick-protobuf` reader
//! (as in the handshake target). The oracle pins the envelope semantics: the map
//! never exceeds the wire entry count, a duplicate key collapses to its
//! last occurrence, and re-encoding the decoded value decodes back equal.
//!
//! Seeds live in `fuzz/seeds/headers_decode/` and are replayed on stable by
//! `seed_replay_headers_decode` in `crates/swarm/net/headers/src/codec.rs`.

#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;
use quick_protobuf::{BytesReader, MessageRead};
use vertex_swarm_net_headers::fuzz::{arbitrary_wire_headers, check_headers, proto};

fuzz_target!(|data: &[u8]| {
    // Flat entry: raw bytes are sound because it carries no nested message.
    let mut reader = BytesReader::from_bytes(data);
    let _ = proto::Header::from_reader(&mut reader, data);

    // Nested envelope: writer-produced bytes with adversarial entries.
    let mut u = Unstructured::new(data);
    if let Ok(wire) = arbitrary_wire_headers(&mut u) {
        let _ = check_headers(wire);
    }
});
