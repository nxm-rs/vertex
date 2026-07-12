//! Fuzz-facing surface behind the `arbitrary` feature: raw decode entry
//! points for the retrieval frames and the address-parameterized delivery
//! round-trip check, shared between the fuzz targets and the stable
//! seed-replay tests. Dev-only; never enabled by a shipped artefact cone.

use asynchronous_codec::{Decoder, Encoder};
use bytes::BytesMut;
use nectar_primitives::ChunkAddress;
use quick_protobuf::{BytesReader, MessageRead};
use vertex_net_codec::ProtoMessage;

use crate::codec::{Delivery, DeliveryCodec, Request};
use crate::protocol::MAX_DELIVERY_SIZE;

pub use vertex_swarm_net_proto::retrieval as proto;

/// Decode a request from raw wire bytes: generated reader, then the domain
/// 32-byte address check.
pub fn decode_request(data: &[u8]) -> Option<Request> {
    let mut reader = BytesReader::from_bytes(data);
    let wire = proto::Request::from_reader(&mut reader, data).ok()?;
    Request::from_proto(wire).ok()
}

/// Decode a delivery fuzz input: the first 32 bytes are the requested
/// address, the rest the raw wire frame, so a fuzzer controls both sides of
/// the chunk reconstruction. Mirrors the production decode (generated
/// reader, then the address-validating conversion) and asserts the invariant
/// a successful decode must uphold: the reconstructed chunk carries the
/// requested address.
pub fn decode_delivery(data: &[u8]) -> Option<Delivery> {
    let (addr, frame) = data.split_first_chunk::<32>()?;
    let expected = ChunkAddress::new(*addr);
    let mut reader = BytesReader::from_bytes(frame);
    let wire = proto::Delivery::from_reader(&mut reader, frame).ok()?;
    let delivery = Delivery::from_proto(wire, expected).ok()?;
    if let Delivery::Chunk { chunk, .. } = &delivery {
        assert_eq!(*chunk.address(), expected);
    }
    Some(delivery)
}

/// Round-trip a delivery through the address-parameterized codec pair at the
/// production frame budget.
///
/// The serve path ships chunk data only, so the oracle compares against the
/// stampless projection of the input; a failure delivery must reproduce
/// itself exactly.
pub fn check_delivery_roundtrip(delivery: Delivery) {
    let expected = match &delivery {
        Delivery::Chunk { chunk, .. } => *chunk.address(),
        Delivery::Error => ChunkAddress::new([0u8; 32]),
    };
    let mut enc = DeliveryCodec::new(MAX_DELIVERY_SIZE, expected);
    let mut buf = BytesMut::new();
    if let Err(e) = enc.encode(delivery.clone(), &mut buf) {
        panic!("valid deliveries must encode: {e}");
    }

    let mut dec = DeliveryCodec::new(MAX_DELIVERY_SIZE, expected);
    let decoded = match dec.decode(&mut buf) {
        Ok(Some(frame)) => frame,
        other => panic!("encoded frames must decode to one whole frame: {other:?}"),
    };
    let stampless = match delivery {
        Delivery::Chunk { chunk, .. } => Delivery::Chunk { chunk, stamp: None },
        Delivery::Error => Delivery::Error,
    };
    assert_eq!(
        decoded, stampless,
        "decode(encode(delivery)) must reproduce the stampless projection"
    );
}
