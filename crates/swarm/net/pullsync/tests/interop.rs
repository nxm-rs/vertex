//! Wire-conformance vectors for the pullsync 1.4.0 messages.
//!
//! These pin the protobuf byte layout every conforming peer must produce. Each
//! domain type is exercised through the public `ProtoMessage` API, never a
//! private reimplementation of the byte layout, and the serialized protobuf
//! bytes are asserted against fixed vectors. A single byte of drift in a field
//! tag, ordering, or the LSB-first `Want` bitvector surfaces as a mismatch.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "conformance fixtures: panicking on malformed test inputs is intended"
)]

use alloy_primitives::{B256, Signature};
use nectar_postage::Stamp;
use nectar_primitives::{AnyChunk, ChunkAddress, ContentChunk};
use quick_protobuf::Writer;
use vertex_net_codec::ProtoMessage;
use vertex_swarm_primitives::{Bin, StampedChunk};

use vertex_swarm_net_pullsync::{Ack, BitVector, ChunkDescriptor, Delivery, Get, Offer, Syn, Want};

/// Serialize a domain message to its raw protobuf bytes (no length framing).
fn proto_bytes<M>(msg: M) -> Vec<u8>
where
    M: ProtoMessage,
    M::EncodeError: std::fmt::Debug,
{
    let proto = msg.into_proto().expect("encode");
    let mut out = Vec::new();
    let mut writer = Writer::new(&mut out);
    quick_protobuf::MessageWrite::write_message(&proto, &mut writer).expect("write");
    out
}

fn descriptor(addr: u8, batch: u8, hash: u8) -> ChunkDescriptor {
    ChunkDescriptor::new(
        ChunkAddress::new([addr; 32]),
        B256::repeat_byte(batch),
        B256::repeat_byte(hash),
    )
}

#[test]
fn syn_encodes_to_empty_message() {
    assert_eq!(proto_bytes(Syn), Vec::<u8>::new());
}

#[test]
fn ack_roundtrips_through_proto() {
    let ack = Ack {
        cursors: vec![0, 5, 0, 9],
        epoch: 42,
    };
    let decoded = Ack::from_proto(ack.clone().into_proto().unwrap()).unwrap();
    assert_eq!(decoded, ack);
}

/// A `Get` with the deepest bin must encode `bin = MAX_PO` (31) and decode back
/// to the same typed bin.
#[test]
fn get_high_bin_roundtrips() {
    let get = Get::new(Bin::MAX, 1_000_000);
    let bytes = proto_bytes(get);
    // field 1 (bin), varint: tag 0x08, value 31 (0x1f).
    // field 2 (start), varint: tag 0x10, value 1_000_000.
    assert_eq!(&bytes[..2], &[0x08, 0x1f]);
    let decoded = Get::from_proto(get.into_proto().unwrap()).unwrap();
    assert_eq!(decoded.bin, Bin::MAX);
    assert_eq!(decoded.start, 1_000_000);
}

/// An offer of two descriptors pins the repeated-`Chunk` layout: each chunk is a
/// length-delimited submessage carrying three 32-byte fields in field order.
#[test]
fn offer_two_descriptors_fixed_bytes() {
    let offer = Offer::new(
        7,
        vec![descriptor(0x11, 0x22, 0x33), descriptor(0x44, 0x55, 0x66)],
    );
    let bytes = proto_bytes(offer.clone());

    // One descriptor's submessage body: field 1 (address) + field 2 (batch_id)
    // + field 3 (stamp_hash), each a 32-byte length-delimited bytes field.
    let chunk_body = |addr: u8, batch: u8, hash: u8| {
        let mut v = Vec::new();
        v.extend_from_slice(&[0x0a, 0x20]); // field 1, len 32
        v.extend_from_slice(&[addr; 32]);
        v.extend_from_slice(&[0x12, 0x20]); // field 2, len 32
        v.extend_from_slice(&[batch; 32]);
        v.extend_from_slice(&[0x1a, 0x20]); // field 3, len 32
        v.extend_from_slice(&[hash; 32]);
        v
    };

    let mut expected = Vec::new();
    // field 1 (topmost), varint 7.
    expected.extend_from_slice(&[0x08, 0x07]);
    // field 2 (chunks), repeated submessage. Each body is 3*(2+32) = 102 = 0x66.
    let first = chunk_body(0x11, 0x22, 0x33);
    let second = chunk_body(0x44, 0x55, 0x66);
    expected.extend_from_slice(&[0x12, first.len() as u8]);
    expected.extend_from_slice(&first);
    expected.extend_from_slice(&[0x12, second.len() as u8]);
    expected.extend_from_slice(&second);

    assert_eq!(bytes, expected, "offer wire layout drifted");

    // And it decodes back to the same descriptors.
    let decoded = Offer::from_proto(offer.clone().into_proto().unwrap()).unwrap();
    assert_eq!(decoded, offer);
}

/// The `Want` bitvector is LSB-first: bit `i` is `0x01 << (i % 8)` in byte
/// `i / 8`, and the byte length is `len / 8 + 1`. Wanting chunks 0, 1, and 8 of
/// a 16-chunk offer encodes the literal bytes `0x03 0x01 0x00` on field 1; the
/// asymmetric bit 1 (`0x02`, not `0x40`) pins the ordering, and the third byte
/// is the `len / 8 + 1` trailing byte.
#[test]
fn want_bitvector_is_lsb_first_fixed_bytes() {
    let mut bv = BitVector::new(16);
    bv.set(0);
    bv.set(1);
    bv.set(8);
    // Byte 0: bits 0,1 -> 0x03. Byte 1: bit 8 -> 0x01. Byte 2: len/8+1 pad -> 0x00.
    assert_eq!(bv.as_bytes(), &[0x03, 0x01, 0x00]);

    let bytes = proto_bytes(Want::new(bv.clone()));
    // field 1 (bit_vector), length-delimited: tag 0x0a, len 3, then 0x03 0x01 0x00.
    assert_eq!(bytes, vec![0x0a, 0x03, 0x03, 0x01, 0x00]);

    // The selection survives a round-trip and counts three wanted chunks.
    let want = Want::new(bv);
    let decoded = Want::from_proto(want.clone().into_proto().unwrap()).unwrap();
    assert_eq!(decoded.count(), 3);
    assert!(decoded.wanted.get(0));
    assert!(decoded.wanted.get(1));
    assert!(decoded.wanted.get(8));
    assert!(!decoded.wanted.get(2));
    assert!(!decoded.wanted.get(7));
}

/// A delivery pins the field order `address(1)`, `data(2)`, `stamp(3)` and
/// reconstructs byte-identically against the chunk's own address.
#[test]
fn delivery_fixed_bytes_and_roundtrip() {
    let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
    let stamp = Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, sig);
    let chunk: AnyChunk = ContentChunk::new(&b"pullsync payload"[..])
        .expect("valid content chunk")
        .into();
    let address = *chunk.address();
    let wire_data = chunk.clone().into_bytes();
    let stamped = StampedChunk::new(chunk, stamp.clone());

    let bytes = proto_bytes(Delivery::new(stamped));

    // Hand-build the expected protobuf: field 1 address (32 bytes), field 2 data,
    // field 3 stamp (113 bytes).
    let mut expected = Vec::new();
    expected.extend_from_slice(&[0x0a, 0x20]);
    expected.extend_from_slice(address.as_bytes());
    expected.push(0x12);
    push_len(&mut expected, wire_data.len());
    expected.extend_from_slice(&wire_data);
    expected.push(0x1a);
    push_len(&mut expected, 113);
    expected.extend_from_slice(&stamp.to_bytes());
    assert_eq!(bytes, expected, "delivery wire layout drifted");

    // Reconstructs to the requested address.
    let proto = vertex_swarm_net_proto::pullsync::Delivery {
        address: address.to_vec(),
        data: wire_data.to_vec(),
        stamp: stamp.to_bytes().to_vec(),
    };
    let decoded = Delivery::from_proto(proto).expect("valid delivery");
    assert_eq!(*decoded.chunk.address(), address);
}

/// Encode a protobuf length as a base-128 varint.
fn push_len(buf: &mut Vec<u8>, mut len: usize) {
    while len >= 0x80 {
        buf.push((len as u8 & 0x7f) | 0x80);
        len >>= 7;
    }
    buf.push(len as u8);
}
