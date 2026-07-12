//! Structured round-trip fuzz of the handshake codec.
//!
//! Inputs come from the shared generator layer: the `arbitrary` feature of
//! `vertex-swarm-net-handshake` builds valid-by-construction frames (a really
//! signed peer record under the generator network id, a wire-representable
//! node type, a welcome message inside the decode cap). The oracle is
//! stronger than "no panic": encode must serialize, the wire bytes must
//! deserialize, and decode must reproduce the original components, signature
//! recovery included. Any failure is a codec bug.

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use quick_protobuf::{BytesReader, MessageRead, MessageWrite, Writer};
use vertex_swarm_net_handshake::fuzz::{
    ARBITRARY_NETWORK_ID, ArbitraryAck, ArbitrarySyn, ArbitrarySynack, decode_ack, decode_syn,
    decode_synack, encode_ack, encode_syn, encode_synack,
};

/// One structured input: any of the three handshake frames, so a single
/// corpus drives all codecs (the ack and synack arms pay key generation and
/// signing per exec, the syn arm stays cheap).
#[derive(Debug, Arbitrary)]
enum HandshakeInput {
    Syn(ArbitrarySyn),
    Ack(ArbitraryAck),
    Synack(ArbitrarySynack),
}

/// Serialize through the generated writer and read back through the
/// generated reader, so the round trip crosses real wire bytes.
fn rewire<M>(wire: M) -> M
where
    M: MessageWrite + for<'a> MessageRead<'a> + std::fmt::Debug,
{
    let mut bytes = Vec::with_capacity(wire.get_size());
    wire.write_message(&mut Writer::new(&mut bytes))
        .expect("proto serialization must succeed");
    let mut reader = BytesReader::from_bytes(&bytes);
    M::from_reader(&mut reader, &bytes).expect("serialized frames must deserialize")
}

fuzz_target!(|input: HandshakeInput| {
    match input {
        HandshakeInput::Syn(syn) => {
            let decoded =
                decode_syn(rewire(encode_syn(&syn.observed))).expect("a well-formed syn decodes");
            assert_eq!(decoded, syn.observed);
        }
        HandshakeInput::Ack(ack) => {
            let wire = rewire(encode_ack(
                &ack.peer,
                ack.node_type,
                &ack.welcome,
                ARBITRARY_NETWORK_ID,
            ));
            let (peer, node_type, welcome) =
                decode_ack(wire, ARBITRARY_NETWORK_ID).expect("a validly signed ack decodes");
            assert_eq!(peer, ack.peer);
            assert_eq!(node_type, ack.node_type);
            assert_eq!(welcome, ack.welcome);
        }
        HandshakeInput::Synack(synack) => {
            let wire = rewire(encode_synack(
                &synack.syn.observed,
                &synack.ack.peer,
                synack.ack.node_type,
                &synack.ack.welcome,
                ARBITRARY_NETWORK_ID,
            ));
            let (observed, peer, node_type, welcome) =
                decode_synack(wire, ARBITRARY_NETWORK_ID).expect("a validly signed synack decodes");
            assert_eq!(observed, synack.syn.observed);
            assert_eq!(peer, synack.ack.peer);
            assert_eq!(node_type, synack.ack.node_type);
            assert_eq!(welcome, synack.ack.welcome);
        }
    }
});
