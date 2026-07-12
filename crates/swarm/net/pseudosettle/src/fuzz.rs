//! Fuzz-facing surface behind the `arbitrary` feature: raw decode entry
//! points for the pseudosettle frames, shared between the fuzz targets and
//! the stable seed-replay test. Dev-only; never enabled by a shipped
//! artefact cone.

use quick_protobuf::{BytesReader, MessageRead};
use vertex_net_codec::{ProtoMessage, encode_u256_be};

use crate::codec::{Payment, PaymentAck};

pub use vertex_swarm_net_proto::pseudosettle as proto;

/// Decode a payment from raw wire bytes: generated reader, then the
/// canonical amount decode. A success must re-encode to the identical wire
/// bytes; a divergence here is an accounting bug, not just a crash.
pub fn decode_payment(data: &[u8]) -> Option<Payment> {
    let mut reader = BytesReader::from_bytes(data);
    let wire = proto::Payment::from_reader(&mut reader, data).ok()?;
    let wire_bytes = wire.amount.clone();
    let decoded = Payment::from_proto(wire).ok()?;
    assert_eq!(
        encode_u256_be(decoded.amount),
        wire_bytes,
        "a decoded amount must re-encode to the identical wire bytes"
    );
    Some(decoded)
}

/// Decode a payment ack from raw wire bytes, holding the same canonical
/// amount invariant as [`decode_payment`]; the timestamp is carried as-is.
pub fn decode_payment_ack(data: &[u8]) -> Option<PaymentAck> {
    let mut reader = BytesReader::from_bytes(data);
    let wire = proto::PaymentAck::from_reader(&mut reader, data).ok()?;
    let wire_bytes = wire.amount.clone();
    let decoded = PaymentAck::from_proto(wire).ok()?;
    assert_eq!(
        encode_u256_be(decoded.amount),
        wire_bytes,
        "a decoded amount must re-encode to the identical wire bytes"
    );
    Some(decoded)
}
