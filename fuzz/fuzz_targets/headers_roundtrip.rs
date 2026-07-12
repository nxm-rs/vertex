//! Structured round-trip fuzz of the headers codec.
//!
//! Inputs come from the shared generator layer: the `arbitrary` feature of
//! `vertex-swarm-net-headers` builds envelopes whose keys are unique by map
//! construction, so this target and the crate's proptest suite drive one
//! construction path. The oracle is stronger than "no panic": `into_proto`
//! must serialize, the wire bytes must deserialize, and `from_proto` must
//! reproduce the original envelope. Any failure is a codec bug.

#![no_main]

use libfuzzer_sys::fuzz_target;
use quick_protobuf::{BytesReader, MessageRead, MessageWrite, Writer};
use vertex_net_codec::ProtoMessage;
use vertex_swarm_net_headers::Headers;

fuzz_target!(|headers: Headers| {
    let wire = match headers.clone().into_proto() {
        Ok(wire) => wire,
        Err(never) => match never {},
    };
    let mut bytes = Vec::with_capacity(wire.get_size());
    wire.write_message(&mut Writer::new(&mut bytes))
        .expect("proto serialization must succeed");
    let mut reader = BytesReader::from_bytes(&bytes);
    let reread = <Headers as ProtoMessage>::Proto::from_reader(&mut reader, &bytes)
        .expect("serialized frames must deserialize");
    let decoded = Headers::from_proto(reread).expect("valid frames must convert");
    assert_eq!(
        decoded, headers,
        "decode(encode(headers)) must reproduce the envelope"
    );
});
