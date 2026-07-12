//! Fuzz-facing surface behind the `arbitrary` feature: the raw decode entry
//! point for the headers envelope, an adversarial wire generator, and the
//! envelope invariant check, shared between the fuzz targets and the stable
//! seed-replay tests. Dev-only; never enabled by a shipped artefact cone.

use arbitrary::Unstructured;
use quick_protobuf::{BytesReader, MessageRead, MessageWrite, Writer};
use vertex_net_codec::ProtoMessage;

use crate::codec::Headers;

pub use vertex_swarm_net_proto::headers as proto;

/// Decode a headers envelope from raw wire bytes: generated reader (which
/// rejects a non-UTF-8 key), then the domain conversion collecting entries
/// into the map.
pub fn decode_headers(data: &[u8]) -> Option<Headers> {
    let mut reader = BytesReader::from_bytes(data);
    let wire = proto::Headers::from_reader(&mut reader, data).ok()?;
    Headers::from_proto(wire).ok()
}

/// A wire envelope with adversarial entries: arbitrary keys and values, a
/// key sometimes deliberately repeating the previous one so the
/// duplicate-key collapse stays exercised.
pub fn arbitrary_wire_headers(u: &mut Unstructured<'_>) -> arbitrary::Result<proto::Headers> {
    let entries: Vec<(String, Vec<u8>)> = u.arbitrary()?;
    let mut headers: Vec<proto::Header> = Vec::with_capacity(entries.len());
    for (key, value) in entries {
        let key = match headers.last() {
            Some(prev) if u.arbitrary()? => prev.key.clone(),
            _ => key,
        };
        headers.push(proto::Header { key, value });
    }
    Ok(proto::Headers { headers })
}

/// Write a wire envelope, read it back, and run the domain conversion,
/// asserting the envelope invariants: the map holds exactly the distinct
/// wire keys, a duplicate key collapses to its last occurrence (proto3
/// last-field-wins), and re-encoding the decoded value decodes back equal.
pub fn check_headers(wire: proto::Headers) -> Headers {
    let mut bytes = Vec::with_capacity(wire.get_size());
    if let Err(e) = wire.write_message(&mut Writer::new(&mut bytes)) {
        panic!("proto serialization must succeed: {e}");
    }
    let Some(decoded) = decode_headers(&bytes) else {
        panic!("writer-produced frames must decode");
    };

    // Insertion order builds the last-wins expectation per key.
    let mut expected = std::collections::HashMap::new();
    for header in &wire.headers {
        expected.insert(&header.key, header.value.as_slice());
    }
    let map = decoded.clone().into_inner();
    assert_eq!(map.len(), expected.len());
    for (key, value) in &expected {
        assert_eq!(
            map.get(*key).map(|v| v.as_ref()),
            Some(*value),
            "a duplicate key must collapse to its last occurrence"
        );
    }

    let reencoded = match decoded.clone().into_proto() {
        Ok(proto) => proto,
        Err(never) => match never {},
    };
    let mut rebytes = Vec::with_capacity(reencoded.get_size());
    if let Err(e) = reencoded.write_message(&mut Writer::new(&mut rebytes)) {
        panic!("proto serialization must succeed: {e}");
    }
    assert_eq!(
        decode_headers(&rebytes),
        Some(decoded.clone()),
        "decode must be stable across re-encode"
    );
    decoded
}
