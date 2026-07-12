//! Fuzz the pushsync wire decoders with raw attacker-controlled bytes.
//!
//! Each input is parsed as both pushsync frames: the generated protobuf
//! struct is deserialized from the raw bytes, then the domain conversion
//! (`ProtoMessage::from_proto`) runs the validation that guards the node:
//! address and nonce lengths, `Stamp::try_from_slice`, chunk reconstruction
//! against the carried address, the 65-byte receipt signature with its
//! empty-signature failure sentinel, and the storage radius bin range. Any
//! returned `Err` is success; the oracle is "no panic, no OOM, no hang".
//!
//! Seeds live in `fuzz/seeds/pushsync_decode/` and are replayed on stable by
//! `seed_replay_pushsync_decode` in `crates/swarm/net/pushsync/src/codec.rs`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use quick_protobuf::{BytesReader, MessageRead};
use vertex_net_codec::ProtoMessage;
use vertex_swarm_net_proto::pushsync as proto;
use vertex_swarm_net_pushsync::{Delivery, ReceiptResponse};

fuzz_target!(|data: &[u8]| {
    let mut reader = BytesReader::from_bytes(data);
    if let Ok(wire) = proto::Delivery::from_reader(&mut reader, data) {
        let _ = Delivery::from_proto(wire);
    }

    let mut reader = BytesReader::from_bytes(data);
    if let Ok(wire) = proto::Receipt::from_reader(&mut reader, data) {
        let _ = ReceiptResponse::from_proto(wire);
    }
});
