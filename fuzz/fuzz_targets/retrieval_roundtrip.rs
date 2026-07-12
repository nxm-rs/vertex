//! Structured round-trip fuzz of the retrieval codec.
//!
//! Inputs come from the shared generator layer: the `arbitrary` feature of
//! `vertex-swarm-net-retrieval` builds valid-by-construction values (a real
//! chunk address for a request; the failure sentinel, a stampless chunk, or
//! a really signed stamped chunk for a delivery), so this target and the
//! crate's proptest suite drive one construction path. The oracle is
//! stronger than "no panic": a request must reproduce itself through the
//! wire, and a delivery must decode back to its stampless projection (the
//! serve path ships chunk data only) through the address-parameterized
//! codec pair. Any failure is a codec bug.

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use quick_protobuf::{BytesReader, MessageRead, MessageWrite, Writer};
use vertex_net_codec::ProtoMessage;
use vertex_swarm_net_retrieval::fuzz::check_delivery_roundtrip;
use vertex_swarm_net_retrieval::{Delivery, Request};

/// One structured input: either retrieval frame, so a single corpus drives
/// both messages (the delivery arm pays chunk construction per exec, the
/// request arm stays cheap).
#[derive(Debug, Arbitrary)]
enum RetrievalInput {
    Request(Request),
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

fuzz_target!(|input: RetrievalInput| {
    match input {
        RetrievalInput::Request(request) => assert_wire_roundtrip(request),
        RetrievalInput::Delivery(delivery) => check_delivery_roundtrip(delivery),
    }
});
