//! Fuzz-facing surface behind the `arbitrary` feature: the raw decode entry
//! point for the pricing frame, shared between the fuzz targets and the
//! stable seed-replay test. Dev-only; never enabled by a shipped artefact
//! cone.

use quick_protobuf::{BytesReader, MessageRead};
use vertex_net_codec::{ProtoMessage, encode_u256_be};

use crate::codec::AnnouncePaymentThreshold;

pub use vertex_swarm_net_proto::pricing as proto;

/// Decode a threshold announcement from raw wire bytes: generated reader,
/// then the canonical amount decode. A success must re-encode to the
/// identical wire bytes; a divergence here is an accounting bug, not just a
/// crash.
pub fn decode_announce(data: &[u8]) -> Option<AnnouncePaymentThreshold> {
    let mut reader = BytesReader::from_bytes(data);
    let wire = proto::AnnouncePaymentThreshold::from_reader(&mut reader, data).ok()?;
    let wire_bytes = wire.payment_threshold.clone();
    let decoded = AnnouncePaymentThreshold::from_proto(wire).ok()?;
    assert_eq!(
        encode_u256_be(decoded.payment_threshold),
        wire_bytes,
        "a decoded threshold must re-encode to the identical wire bytes"
    );
    Some(decoded)
}
