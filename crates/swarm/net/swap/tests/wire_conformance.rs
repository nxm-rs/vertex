//! Wire-conformance vectors for the SWAP protocol frames.
//!
//! The `/swarm/swap/1.0.0/swap` protocol carries two length-delimited protobuf
//! frames over a headered stream:
//!
//! - `EmitCheque { bytes cheque = 1 }`, where `cheque` is the fixed-shape JSON
//!   encoding of a `SignedCheque` (see the chequebook crate's
//!   `json_conformance` vectors for the pinned JSON bytes).
//! - `Handshake { bytes beneficiary = 1 }`, a raw 20-byte address.
//!
//! These vectors pin the exact bytes that go on the wire so the codec cannot
//! drift. The framing is an unsigned-varint length prefix followed by the
//! protobuf message; the cheque JSON is byte-identical to the live network.
//! The expected bytes are constructed independently of the codec here, so a
//! codec change that alters the wire output fails the assertion rather than
//! quietly moving the vector.
#![allow(clippy::unwrap_used)]

use alloy_primitives::{Address, U256};
use asynchronous_codec::{Decoder, Encoder};
use bytes::{Bytes, BytesMut};
use vertex_swarm_bandwidth_chequebook::{Cheque, ChequeExt, SignedCheque};
use vertex_swarm_net_swap::{EmitCheque, EmitChequeCodec, Handshake, HandshakeCodec};

/// The JSON encoding of the cheque used in the `EmitCheque` vector below. This
/// string is byte-identical to the chequebook crate's first conformance vector
/// (sequential signature bytes `0..=64`, payout `1000000`).
const CHEQUE_JSON: &str = "{\"Chequebook\":\"0x0101010101010101010101010101010101010101\",\"Beneficiary\":\"0x0202020202020202020202020202020202020202\",\"CumulativePayout\":1000000,\"Signature\":\"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKissLS4vMDEyMzQ1Njc4OTo7PD0+P0A=\"}";

/// Build the signed cheque whose JSON encoding is [`CHEQUE_JSON`].
fn vector_cheque() -> SignedCheque {
    SignedCheque::new(
        Cheque::new(
            Address::repeat_byte(0x01),
            Address::repeat_byte(0x02),
            U256::from(1_000_000u64),
        ),
        Bytes::from((0u8..=64).collect::<Vec<u8>>()),
    )
}

/// Prepend an unsigned-varint length prefix to a protobuf message body,
/// matching the framing applied by the codec.
fn frame(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 2);
    let mut len = body.len();
    loop {
        let mut byte = (len & 0x7f) as u8;
        len >>= 7;
        if len != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if len == 0 {
            break;
        }
    }
    out.extend_from_slice(body);
    out
}

/// Encode a single length-delimited protobuf field (`tag`, varint length,
/// payload) for field number 1, wire type 2.
fn field1_len_delimited(payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0x0a]; // field 1, wire type 2 (length-delimited)
    let mut len = payload.len();
    loop {
        let mut byte = (len & 0x7f) as u8;
        len >>= 7;
        if len != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if len == 0 {
            break;
        }
    }
    out.extend_from_slice(payload);
    out
}

#[test]
fn emit_cheque_frame_matches_pinned_bytes() {
    // The cheque JSON must match the chequebook vector byte-for-byte.
    assert_eq!(
        core::str::from_utf8(&vector_cheque().to_json().unwrap()).unwrap(),
        CHEQUE_JSON,
        "cheque JSON diverged from the chequebook conformance vector"
    );

    let proto_body = field1_len_delimited(CHEQUE_JSON.as_bytes());
    let expected = frame(&proto_body);

    let mut codec = EmitChequeCodec::new(8192);
    let mut buf = BytesMut::new();
    codec
        .encode(EmitCheque::new(vector_cheque()), &mut buf)
        .unwrap();

    assert_eq!(
        buf.as_ref(),
        expected.as_slice(),
        "EmitCheque frame diverged from the pinned wire bytes"
    );

    let decoded = codec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(decoded.cheque, vector_cheque());
}

#[test]
fn handshake_frame_matches_pinned_bytes() {
    let beneficiary = Address::repeat_byte(0x42);

    let proto_body = field1_len_delimited(beneficiary.as_slice());
    let expected = frame(&proto_body);

    let mut codec = HandshakeCodec::new(1024);
    let mut buf = BytesMut::new();
    codec.encode(Handshake::new(beneficiary), &mut buf).unwrap();

    assert_eq!(
        buf.as_ref(),
        expected.as_slice(),
        "Handshake frame diverged from the pinned wire bytes"
    );

    let decoded = codec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(decoded.beneficiary, beneficiary);
}

#[test]
fn decode_from_pinned_emit_cheque_bytes() {
    let proto_body = field1_len_delimited(CHEQUE_JSON.as_bytes());
    let wire = frame(&proto_body);

    let mut codec = EmitChequeCodec::new(8192);
    let mut buf = BytesMut::from(wire.as_slice());
    let decoded = codec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(decoded.cheque, vector_cheque());
}

#[test]
fn decode_rejects_short_beneficiary() {
    // A beneficiary that is not 20 bytes must be rejected by the codec.
    let proto_body = field1_len_delimited(&[0x01, 0x02, 0x03]);
    let wire = frame(&proto_body);

    let mut codec = HandshakeCodec::new(1024);
    let mut buf = BytesMut::from(wire.as_slice());
    assert!(codec.decode(&mut buf).is_err());
}
