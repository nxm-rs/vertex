//! Fuzz-facing surface behind the `arbitrary` feature: raw decode entry
//! points for the swap frames, shared between the fuzz targets and the
//! stable seed-replay test. Dev-only; never enabled by a shipped artefact
//! cone.

use quick_protobuf::{BytesReader, MessageRead};
use vertex_net_codec::ProtoMessage;

use crate::codec::{EmitCheque, Handshake};

pub use vertex_swarm_net_proto::swap as proto;

/// Decode an emitted cheque from raw wire bytes: generated reader, then the
/// JSON cheque payload parse (addresses, the bare-decimal payout across the
/// full 256-bit range, the fixed 65-byte base64 signature). The JSON is
/// value-preserving rather than byte-canonical, so a success must survive a
/// re-encode then decode unchanged; a divergence here is an accounting bug,
/// not just a crash.
pub fn decode_emit_cheque(data: &[u8]) -> Option<EmitCheque> {
    let mut reader = BytesReader::from_bytes(data);
    let wire = proto::EmitCheque::from_reader(&mut reader, data).ok()?;
    let decoded = EmitCheque::from_proto(wire).ok()?;
    let reencoded = match decoded.clone().into_proto() {
        Ok(proto) => proto,
        Err(e) => panic!("a decoded cheque must re-encode: {e}"),
    };
    match EmitCheque::from_proto(reencoded) {
        Ok(redecoded) => assert_eq!(redecoded, decoded, "decode must be stable across re-encode"),
        Err(e) => panic!("a re-encoded cheque must decode: {e}"),
    }
    Some(decoded)
}

/// Decode a swap handshake from raw wire bytes: generated reader, then the
/// 20-byte beneficiary length check.
pub fn decode_handshake(data: &[u8]) -> Option<Handshake> {
    let mut reader = BytesReader::from_bytes(data);
    let wire = proto::Handshake::from_reader(&mut reader, data).ok()?;
    Handshake::from_proto(wire).ok()
}
