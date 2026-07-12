//! Fuzz the handshake domain decoders, the deepest untrusted boundary: they
//! run EIP-191 signature recovery and overlay validation on fields from a peer
//! that has not yet been admitted.
//!
//! Each input drives `decode_syn`, `decode_ack`, and `decode_synack` with
//! attacker-shaped field content: wrong-length overlay/nonce/signature,
//! malformed multiaddr blocks (exercising the hand-rolled list/uvarint decoder
//! and its count cap), an over-long welcome message, and a mismatched network
//! id. The nested frames go through the real write then read then decode path,
//! so the generated reader is exercised on writer-produced bytes while the
//! domain decode sees hostile content; the flat `Syn` frame is additionally
//! fed raw bytes straight through the generated reader. Half the records are
//! really signed, so signature recovery succeeds on that arm, and a decoded
//! identity is pushed through the admission gate the protocol consults after
//! decode. Any returned `Err` is success; the oracle is "no panic, no OOM, no
//! hang".
//!
//! The generated protobuf reader on raw bytes is upstream code; a separate
//! nested-length soundness issue in `quick-protobuf` was found and reported,
//! so this target drives the domain conversion, which is this workspace's code
//! and the actual attack surface. Seeds live in `fuzz/seeds/handshake_decode/`
//! and are replayed on stable by `seed_replay_handshake_decode` in
//! `crates/swarm/net/handshake/src/codec/mod.rs`.

#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;
use quick_protobuf::{BytesReader, MessageRead, MessageWrite, Writer};
use vertex_swarm_net_handshake::fuzz::{
    ARBITRARY_NETWORK_ID, NetworkId, SwarmNodeType, SwarmPeer, arbitrary_wire_ack,
    arbitrary_wire_synack, decode_ack, decode_syn, decode_synack, proto,
};
use vertex_swarm_net_handshake::{
    AdmissionDecision, AlwaysAccept, ConnectionDirection, HandshakeAdmissionControl,
};

/// Serialize a proto struct through the generated writer. A writer never emits
/// an overrunning nested length, so reading the result back is sound.
fn wire<M: MessageWrite>(msg: &M) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(msg.get_size());
    msg.write_message(&mut Writer::new(&mut bytes))
        .expect("proto serialization must succeed");
    bytes
}

/// Run the admission gate the protocol consults on a decoded identity.
fn admit(peer: &SwarmPeer, node_type: SwarmNodeType) {
    for direction in [ConnectionDirection::Inbound, ConnectionDirection::Outbound] {
        assert!(matches!(
            AlwaysAccept.evaluate(peer.overlay(), node_type, direction),
            AdmissionDecision::Accept
        ));
    }
}

fuzz_target!(|data: &[u8]| {
    // Flat frame: raw attacker bytes straight through the generated reader are
    // sound because `Syn` carries no nested message.
    let mut reader = BytesReader::from_bytes(data);
    if let Ok(syn) = proto::Syn::from_reader(&mut reader, data) {
        let _ = decode_syn(syn);
    }

    let mut u = Unstructured::new(data);

    if let Ok((ack, echoed)) = arbitrary_wire_ack(&mut u) {
        let bytes = wire(&ack);
        let mut reader = BytesReader::from_bytes(&bytes);
        if let Ok(wire_ack) = proto::Ack::from_reader(&mut reader, &bytes) {
            if let Ok((peer, node_type, _welcome)) = decode_ack(wire_ack.clone(), echoed) {
                admit(&peer, node_type);
            }
            let _ = decode_ack(wire_ack, ARBITRARY_NETWORK_ID);
        }
    }

    if let Ok((synack, echoed)) = arbitrary_wire_synack(&mut u) {
        let bytes = wire(&synack);
        let mut reader = BytesReader::from_bytes(&bytes);
        if let Ok(wire_synack) = proto::SynAck::from_reader(&mut reader, &bytes) {
            if let Ok((_observed, peer, node_type, _welcome)) =
                decode_synack(wire_synack.clone(), echoed)
            {
                admit(&peer, node_type);
            }
            let _ = decode_synack(wire_synack, NetworkId::new(0));
        }
    }
});
