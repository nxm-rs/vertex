//! Feature-gated [`arbitrary::Arbitrary`] impls for the vertex-owned
//! primitive types.
//!
//! Valid by construction: every generated value satisfies the invariants the
//! public constructors enforce (in-range bins, really signed chunks and
//! stamps via nectar's valid-tier generators), so proptest strategies and
//! fuzz targets drive one construction path. Nectar-owned types keep their
//! upstream impls; only the vertex-owned wrappers live here.

use arbitrary::{Arbitrary, Unstructured};
use nectar_postage::generators::signed_stamped_chunk;
use nectar_primitives::generators::{content_chunk, single_owner_chunk};
use nectar_primitives::{AnyChunk, Bin, ChunkTypeId, ChunkTypeSet, DEFAULT_BODY_SIZE};

use crate::{
    CachedChunk, NeighborhoodDepth, StorageRadius, SwarmNodeType, ValidatedChunk,
    VerifiedStampedChunk,
};

impl<'a> Arbitrary<'a> for NeighborhoodDepth {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        Bin::arbitrary(u).map(Self::new)
    }

    fn size_hint(depth: usize) -> (usize, Option<usize>) {
        Bin::size_hint(depth)
    }
}

impl<'a> Arbitrary<'a> for StorageRadius {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        Bin::arbitrary(u).map(Self::new)
    }

    fn size_hint(depth: usize) -> (usize, Option<usize>) {
        Bin::size_hint(depth)
    }
}

impl<'a> Arbitrary<'a> for SwarmNodeType {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        u.choose(&[Self::Bootnode, Self::Client, Self::Storer])
            .copied()
    }
}

impl<'a> Arbitrary<'a> for VerifiedStampedChunk {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        use crate::StampedChunkExt;
        let (stamped, _batch) = signed_stamped_chunk::<DEFAULT_BODY_SIZE>(u)?;
        let address = *stamped.address();
        // Verification against the chunk's own address cannot fail.
        stamped
            .verify_answers(address)
            .map_err(|_| arbitrary::Error::IncorrectFormat)
    }
}

impl<'a> Arbitrary<'a> for CachedChunk {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        let (stamped, _batch) = signed_stamped_chunk::<DEFAULT_BODY_SIZE>(u)?;
        let (chunk, stamp) = stamped.into_parts();
        // A cached single-owner chunk always carries the stamp that orders
        // its versions; a content chunk may be cached stampless.
        let stamp = (chunk.is_single_owner() || u.arbitrary()?).then_some(stamp);
        Ok(Self::new(chunk, stamp))
    }
}

impl<'a, C: ChunkTypeSet> Arbitrary<'a> for ValidatedChunk<C> {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        let constructable: Vec<ChunkTypeId> = C::supported_types()
            .iter()
            .copied()
            .filter(|&id| id == ChunkTypeId::CONTENT || id == ChunkTypeId::SINGLE_OWNER)
            .collect();
        let chunk: AnyChunk = if *u.choose(&constructable)? == ChunkTypeId::CONTENT {
            content_chunk::<DEFAULT_BODY_SIZE>(u)?.into()
        } else {
            single_owner_chunk::<DEFAULT_BODY_SIZE>(u)?.into()
        };
        Self::new(chunk).map_err(|_| arbitrary::Error::IncorrectFormat)
    }
}

#[cfg(test)]
mod tests {
    use nectar_primitives::{ContentOnlyChunkSet, StandardChunkSet};

    use super::*;

    /// A deterministic byte pool with enough entropy for chunk generation.
    fn pool() -> Vec<u8> {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        (0..8192)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                (state >> 56) as u8
            })
            .collect()
    }

    #[test]
    fn depth_and_radius_stay_in_bin_range() {
        let bytes = pool();
        let mut u = Unstructured::new(&bytes);
        for _ in 0..64 {
            assert!(NeighborhoodDepth::arbitrary(&mut u).unwrap().get() <= Bin::MAX.get());
            assert!(StorageRadius::arbitrary(&mut u).unwrap().get() <= Bin::MAX.get());
        }
    }

    #[test]
    fn verified_stamped_chunk_answers_its_own_address() {
        let bytes = pool();
        let mut u = Unstructured::new(&bytes);
        let verified = VerifiedStampedChunk::arbitrary(&mut u).unwrap();
        assert_eq!(verified.address(), verified.stamped().address());
    }

    #[test]
    fn cached_single_owner_chunk_always_carries_a_stamp() {
        let bytes = pool();
        let mut u = Unstructured::new(&bytes);
        let mut seen_soc = false;
        while !u.is_empty() {
            let Ok(cached) = CachedChunk::arbitrary(&mut u) else {
                break;
            };
            if cached.chunk().is_single_owner() {
                seen_soc = true;
                assert!(cached.stamp().is_some(), "a cached SOC must be stamped");
            }
        }
        assert!(seen_soc, "the pool must produce at least one SOC");
    }

    #[test]
    fn validated_chunk_respects_the_type_set() {
        let bytes = pool();
        let mut u = Unstructured::new(&bytes);
        let content_only = ValidatedChunk::<ContentOnlyChunkSet>::arbitrary(&mut u).unwrap();
        assert_eq!(content_only.type_id(), ChunkTypeId::CONTENT);
        let standard = ValidatedChunk::<StandardChunkSet>::arbitrary(&mut u).unwrap();
        assert!(<StandardChunkSet as ChunkTypeSet>::supports(
            standard.type_id()
        ));
    }
}
