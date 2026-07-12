//! Structured round-trip fuzz of the pushsync codec.
//!
//! Inputs are valid by construction: a delivery carries a chunk from the
//! valid-tier nectar generators (content, or single-owner signed by a key
//! drawn from the same input) paired with a raw-tier arbitrary stamp (the
//! codec carries the stamp opaquely), and a receipt carries a well-formed
//! signature, a 32-byte nonce, and an in-range storage radius. The oracle is
//! stronger than "no panic": `into_proto` must serialize, the wire bytes must
//! deserialize, and `from_proto` must reproduce the original value. Any
//! failure is a codec bug.
//!
//! The `Arbitrary` impl here is a local bootstrap; the shared generator layer
//! replaces it once the workspace grows one.

#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;
use nectar_primitives::{Bin, ChunkAddress, DEFAULT_BODY_SIZE, Nonce, generators};
use quick_protobuf::{BytesReader, MessageRead, MessageWrite, Writer};
use vertex_net_codec::ProtoMessage;
use vertex_swarm_net_pushsync::{Delivery, ReceiptResponse, WireReceipt};
use vertex_swarm_primitives::{StampedChunk, StorageRadius};

/// One structured input: either pushsync frame, so a single corpus drives
/// both codecs (the delivery arm pays chunk construction per exec, the
/// receipt arms stay cheap).
#[derive(Debug)]
enum PushsyncInput {
    Delivery(Delivery),
    Receipt(ReceiptResponse),
}

impl<'a> Arbitrary<'a> for PushsyncInput {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        if u.arbitrary()? {
            let chunk = generators::any_chunk::<DEFAULT_BODY_SIZE>(u)?;
            let stamp = nectar_postage::Stamp::arbitrary(u)?;
            Ok(Self::Delivery(Delivery::new(StampedChunk::new(
                chunk, stamp,
            ))))
        } else if u.arbitrary()? {
            let address = ChunkAddress::new(u.arbitrary()?);
            let signature = alloy_primitives::Signature::new(
                alloy_primitives::U256::from_be_bytes(u.arbitrary::<[u8; 32]>()?),
                alloy_primitives::U256::from_be_bytes(u.arbitrary::<[u8; 32]>()?),
                u.arbitrary()?,
            );
            let nonce = Nonce::new(u.arbitrary()?);
            let radius = StorageRadius::new(
                Bin::new(u.int_in_range(0..=31)?).map_err(|_| arbitrary::Error::IncorrectFormat)?,
            );
            Ok(Self::Receipt(ReceiptResponse::Stored(WireReceipt::new(
                address, signature, nonce, radius,
            ))))
        } else {
            Ok(Self::Receipt(ReceiptResponse::Failed))
        }
    }
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

fuzz_target!(|input: PushsyncInput| {
    match input {
        PushsyncInput::Delivery(delivery) => assert_wire_roundtrip(delivery),
        PushsyncInput::Receipt(receipt) => assert_wire_roundtrip(receipt),
    }
});
