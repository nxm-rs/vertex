//! Structured round-trip fuzz of the swap codec.
//!
//! Inputs come from the shared generator layer: the `arbitrary` feature of
//! `vertex-swarm-net-swap` builds valid-by-construction cheques (any 65
//! signature bytes: the codec carries them opaquely) and handshakes, so
//! this target and the crate's proptest suite drive one construction path.
//! The cheque payload rides as transport JSON, so the round-trip pins the
//! full 256-bit payout surviving the bare-decimal number path. The oracle
//! is stronger than "no panic": encode must decode back to an equal value.
//! Any failure is an accounting bug.

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use quick_protobuf::{BytesReader, MessageRead, MessageWrite, Writer};
use vertex_net_codec::ProtoMessage;
use vertex_swarm_net_swap::{EmitCheque, Handshake};

/// One structured input: either swap frame, so a single corpus drives both
/// messages (the cheque arm pays JSON encode/decode per exec, the handshake
/// arm stays cheap).
#[derive(Debug, Arbitrary)]
enum SwapInput {
    EmitCheque(EmitCheque),
    Handshake(Handshake),
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

fuzz_target!(|input: SwapInput| {
    match input {
        SwapInput::EmitCheque(cheque) => assert_wire_roundtrip(cheque),
        SwapInput::Handshake(handshake) => assert_wire_roundtrip(handshake),
    }
});
