//! Structured round-trip fuzz of the pricing codec.
//!
//! Inputs come from the shared generator layer: the `arbitrary` feature of
//! `vertex-swarm-net-pricing` builds valid-by-construction announcements
//! across the full 256-bit threshold range, so this target and the crate's
//! proptest suite drive one construction path. The oracle is stronger than
//! "no panic": encode must decode back to an equal value. Any failure is an
//! accounting bug.

#![no_main]

use libfuzzer_sys::fuzz_target;
use quick_protobuf::{BytesReader, MessageRead, MessageWrite, Writer};
use vertex_net_codec::ProtoMessage;
use vertex_swarm_net_pricing::AnnouncePaymentThreshold;

fuzz_target!(|announce: AnnouncePaymentThreshold| {
    let wire = match announce.clone().into_proto() {
        Ok(wire) => wire,
        Err(never) => match never {},
    };
    let mut bytes = Vec::with_capacity(wire.get_size());
    wire.write_message(&mut Writer::new(&mut bytes))
        .expect("proto serialization must succeed");
    let mut reader = BytesReader::from_bytes(&bytes);
    let reread =
        <AnnouncePaymentThreshold as ProtoMessage>::Proto::from_reader(&mut reader, &bytes)
            .expect("serialized frames must deserialize");
    let decoded = AnnouncePaymentThreshold::from_proto(reread).expect("valid frames must convert");
    assert_eq!(
        decoded, announce,
        "decode(encode(value)) must reproduce the value"
    );
});
