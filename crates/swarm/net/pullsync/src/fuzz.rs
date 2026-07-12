//! Fuzz-facing surface behind the `arbitrary` feature: an adversarial wire
//! `Offer` generator and the bitvector invariant check, shared between the
//! fuzz targets and the stable seed-replay tests. Dev-only; never enabled by
//! a shipped artefact cone.

use arbitrary::{Arbitrary, Unstructured};
use vertex_net_codec::ProtoMessage;

use crate::bitvector::{BitVector, BitVectorError};
use crate::codec::Offer;

pub use vertex_swarm_net_proto::pullsync as proto;

/// A wire `Offer` with adversarial descriptor content: the outbound mapping
/// of a valid offer with descriptors randomly replaced by hostile field
/// bytes, so the nested-message frame stays writer-produced while
/// `from_proto` sees hostile input.
pub fn arbitrary_wire_offer(u: &mut Unstructured<'_>) -> arbitrary::Result<proto::Offer> {
    let mut wire = Offer::arbitrary(u)?.into_proto().map_err(|e| match e {})?;
    for chunk in &mut wire.chunks {
        if u.arbitrary()? {
            *chunk = proto::Chunk {
                address: u.arbitrary()?,
                batch_id: u.arbitrary()?,
                stamp_hash: u.arbitrary()?,
            };
        }
    }
    Ok(wire)
}

/// Drive the bitvector decode surface with raw fuzz bytes.
///
/// The whole input is first taken as wire bytes (the `Want` path, where byte
/// length is the only length signal). If the input then carries a two-byte
/// little-endian length prefix, the remainder goes through
/// [`BitVector::from_bytes`] against that declared selection length; the
/// outcome is returned so the seed-replay test can classify seeds.
///
/// Asserted invariants: `from_bytes` accepts exactly `len / 8 + 1` bytes and
/// reports both lengths on rejection, trailing pad bits never surface through
/// `get`/`count_ones`, `set` at or past `len` is a no-op, and accepted bytes
/// round-trip unchanged through `into_bytes`/`from_bytes`.
pub fn check_bitvector(data: &[u8]) -> Option<Result<BitVector, BitVectorError>> {
    let wire = BitVector::from_wire_bytes(data.to_vec());
    assert_eq!(wire.len(), data.len() * 8);
    assert_eq!(wire.as_bytes(), data);
    assert_eq!(wire.is_empty(), data.is_empty());
    assert!(wire.count_ones() <= wire.len());

    let (len_bytes, rest) = data.split_first_chunk::<2>()?;
    let len = usize::from(u16::from_le_bytes(*len_bytes));
    let outcome = BitVector::from_bytes(rest.to_vec(), len);
    match &outcome {
        Ok(bv) => {
            assert_eq!(rest.len(), len / 8 + 1);
            assert_eq!(bv.len(), len);
            assert_eq!(bv.as_bytes(), rest);
            for i in len..rest.len() * 8 {
                assert!(!bv.get(i), "pad bit {i} must stay invisible");
            }
            assert!(bv.count_ones() <= len);

            let mut poked = bv.clone();
            poked.set(len);
            assert_eq!(&poked, bv, "set past len must be a no-op");

            let roundtripped = BitVector::from_bytes(bv.clone().into_bytes(), len);
            assert_eq!(roundtripped.as_ref(), Ok(bv));
        }
        Err(e) => {
            assert_ne!(rest.len(), len / 8 + 1);
            assert_eq!(e.expected, len / 8 + 1);
            assert_eq!(e.got, rest.len());
        }
    }
    Some(outcome)
}
