//! Structured round-trip fuzz of the pullsync codec.
//!
//! Inputs come from the shared generator layer: the `arbitrary` feature of
//! `vertex-swarm-net-pullsync` builds valid-by-construction values (a really
//! signed stamped chunk for a delivery, byte-aligned wire bytes for a want),
//! so this target and the crate's proptest suite drive one construction
//! path. The oracle is stronger than "no panic": `into_proto` must
//! serialize, the wire bytes must deserialize, and `from_proto` must
//! reproduce the original value. Any failure is a codec bug.

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use quick_protobuf::{BytesReader, MessageRead, MessageWrite, Writer};
use vertex_net_codec::ProtoMessage;
use vertex_swarm_net_pullsync::{Ack, Delivery, Get, Offer, Syn, Want};

/// One structured input: any pullsync frame, so a single corpus drives the
/// whole message set (the delivery arm pays chunk construction per exec, the
/// other arms stay cheap).
#[derive(Debug, Arbitrary)]
enum PullsyncInput {
    Syn(Syn),
    Ack(Ack),
    Get(Get),
    Offer(Offer),
    Want(Want),
    Delivery(Delivery),
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

fuzz_target!(|input: PullsyncInput| {
    match input {
        PullsyncInput::Syn(syn) => assert_wire_roundtrip(syn),
        PullsyncInput::Ack(ack) => assert_wire_roundtrip(ack),
        PullsyncInput::Get(get) => assert_wire_roundtrip(get),
        PullsyncInput::Offer(offer) => assert_wire_roundtrip(offer),
        PullsyncInput::Want(want) => assert_wire_roundtrip(want),
        PullsyncInput::Delivery(delivery) => assert_wire_roundtrip(delivery),
    }
});
