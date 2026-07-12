//! Structured round-trip fuzz of the pushsync codec.
//!
//! Inputs come from the shared generator layer: the `arbitrary` feature of
//! `vertex-swarm-net-pushsync` builds valid-by-construction values (a really
//! signed stamped chunk for a delivery, a real signature over the chunk
//! address for a receipt), so this target and the crate's proptest suite
//! drive one construction path. The oracle is stronger than "no panic":
//! `into_proto` must serialize, the wire bytes must deserialize, and
//! `from_proto` must reproduce the original value. Any failure is a codec
//! bug.

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use quick_protobuf::{BytesReader, MessageRead, MessageWrite, Writer};
use vertex_net_codec::ProtoMessage;
use vertex_swarm_net_pushsync::{Delivery, ReceiptResponse};

/// One structured input: either pushsync frame, so a single corpus drives
/// both codecs (the delivery arm pays chunk construction per exec, the
/// receipt arms stay cheap).
#[derive(Debug, Arbitrary)]
enum PushsyncInput {
    Delivery(Delivery),
    Receipt(ReceiptResponse),
}

/// Encode through the generated writer, decode back through the generated
/// reader, then assert the domain conversion reproduces the value.
fn assert_wire_roundtrip<M>(value: M)
where
    M: ProtoMessage + Clone + PartialEq + std::fmt::Debug,
    M::EncodeError: std::fmt::Debug,
    M::DecodeError: std::fmt::Debug,
{
    let wire = value
        .clone()
        .into_proto()
        .expect("valid values must encode");
    let mut bytes = Vec::with_capacity(wire.get_size());
    wire.write_message(&mut Writer::new(&mut bytes))
        .expect("proto serialization must succeed");
    let mut reader = BytesReader::from_bytes(&bytes);
    let reread =
        M::Proto::from_reader(&mut reader, &bytes).expect("serialized frames must deserialize");
    let decoded = M::from_proto(reread).expect("valid frames must convert");
    assert_eq!(
        decoded, value,
        "decode(encode(value)) must reproduce the value"
    );
}

fuzz_target!(|input: PushsyncInput| {
    match input {
        PushsyncInput::Delivery(delivery) => assert_wire_roundtrip(delivery),
        PushsyncInput::Receipt(receipt) => assert_wire_roundtrip(receipt),
    }
});
