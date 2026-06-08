//! Wire-conformance vectors for the SWAP protocol frames.
//!
//! The `/swarm/swap/1.0.0/swap` protocol carries two length-delimited protobuf
//! frames over a headered stream:
//!
//! - `EmitCheque { bytes cheque = 1 }`, where `cheque` is the JSON encoding of a
//!   `SignedCheque`. The cheque JSON is transport-only (the signature is EIP-712
//!   over the cheque fields, not over the JSON bytes), so these tests pin the
//!   protobuf framing and the embedded-bytes round-trip, not the cheque JSON
//!   content; the chequebook crate's `json_conformance` tests cover the cheque
//!   shape semantically.
//! - `Handshake { bytes beneficiary = 1 }`, a raw 20-byte address.
//!
//! The framing is an unsigned-varint length prefix followed by the protobuf
//! message. The expected bytes are constructed independently of the codec here
//! from the actual cheque JSON, so a framing change fails the assertion rather
//! than quietly moving the vector.
#![allow(clippy::unwrap_used)]

use alloy_primitives::{Address, U256};
use asynchronous_codec::{Decoder, Encoder};
use bytes::{Bytes, BytesMut};
use vertex_swarm_bandwidth_chequebook::{Cheque, ChequeExt, SignedCheque};
use vertex_swarm_net_swap::{EmitCheque, EmitChequeCodec, Handshake, HandshakeCodec};

/// Build the signed cheque used in the `EmitCheque` framing tests.
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
fn emit_cheque_frame_matches_protobuf_framing() {
    // The cheque JSON content is validated semantically in the chequebook crate.
    // Here we pin only the protobuf framing around whatever JSON is produced.
    let cheque_json = serde_json::to_vec(&vector_cheque()).unwrap();
    let proto_body = field1_len_delimited(&cheque_json);
    let expected = frame(&proto_body);

    let mut codec = EmitChequeCodec::new(8192);
    let mut buf = BytesMut::new();
    codec
        .encode(EmitCheque::new(vector_cheque()), &mut buf)
        .unwrap();

    assert_eq!(
        buf.as_ref(),
        expected.as_slice(),
        "EmitCheque frame diverged from the expected protobuf framing"
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
fn decode_from_framed_emit_cheque_bytes() {
    let cheque_json = serde_json::to_vec(&vector_cheque()).unwrap();
    let proto_body = field1_len_delimited(&cheque_json);
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
