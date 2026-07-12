//! Fuzz the pullsync wire decoders with raw attacker-controlled bytes.
//!
//! Each input is parsed as every flat pullsync frame: the generated protobuf
//! struct is deserialized from the raw bytes, then the domain conversion
//! (`ProtoMessage::from_proto`) runs the validation that guards the node:
//! the bin range on `Get`, the implicit byte-length sizing of the `Want`
//! bitvector, and the address length, `Stamp::try_from_slice`, and chunk
//! reconstruction on `Delivery`. The nested `Offer` frame goes through the
//! real write then read path with adversarial descriptor field content, so
//! the domain decode (the 32-byte descriptor field checks) sees hostile
//! input while the reader stays on writer-produced bytes. The `Offer` is not
//! raw-fed pending the reported nested-length soundness issue in the upstream
//! `quick-protobuf` reader (as in the handshake target). Any returned `Err`
//! is success; the oracle is "no panic, no OOM, no hang".
//!
//! Seeds live in `fuzz/seeds/pullsync_decode/` and are replayed on stable by
//! `seed_replay_pullsync_decode` in `crates/swarm/net/pullsync/src/codec.rs`.

#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;
use quick_protobuf::{BytesReader, MessageRead};
use vertex_net_codec::ProtoMessage;
use vertex_swarm_net_pullsync::fuzz::{arbitrary_wire_offer, proto};
use vertex_swarm_net_pullsync::{Ack, Delivery, Get, Offer, Syn, Want};

/// One flat-frame arm: raw attacker bytes straight through the generated
/// reader are sound because the frame carries no nested message.
fn decode<M: ProtoMessage>(data: &[u8]) {
    let mut reader = BytesReader::from_bytes(data);
    if let Ok(wire) = M::Proto::from_reader(&mut reader, data) {
        let _ = M::from_proto(wire);
    }
}

fuzz_target!(|data: &[u8]| {
    decode::<Syn>(data);
    decode::<Ack>(data);
    decode::<Get>(data);
    decode::<Want>(data);
    decode::<Delivery>(data);

    // Nested frame: writer-produced bytes with adversarial descriptor content.
    let mut u = Unstructured::new(data);
    if let Ok(offer) = arbitrary_wire_offer(&mut u) {
        use quick_protobuf::{MessageWrite, Writer};
        let mut bytes = Vec::with_capacity(offer.get_size());
        offer
            .write_message(&mut Writer::new(&mut bytes))
            .expect("proto serialization must succeed");
        let mut reader = BytesReader::from_bytes(&bytes);
        let wire = proto::Offer::from_reader(&mut reader, &bytes)
            .expect("serialized frames must deserialize");
        let _ = Offer::from_proto(wire);
    }
});
