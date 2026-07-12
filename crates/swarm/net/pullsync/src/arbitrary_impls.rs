//! Feature-gated [`arbitrary::Arbitrary`] impls for the pullsync wire types.
//!
//! Valid by construction: a delivery carries a really signed stamped chunk
//! from nectar's valid-tier generators, and a want wraps whole wire bytes
//! (the frame carries no explicit chunk count, so byte-aligned vectors are
//! the ones whose identity survives the round-trip), so the proptest suite
//! and the fuzz round-trip target drive one construction path.

use alloy_primitives::B256;
use arbitrary::{Arbitrary, Unstructured};
use nectar_primitives::{ChunkAddress, DEFAULT_BODY_SIZE};
use vertex_swarm_primitives::{BatchId, Bin};

use crate::bitvector::BitVector;
use crate::codec::{Ack, ChunkDescriptor, Delivery, Get, Offer, Syn, Want};

impl<'a> Arbitrary<'a> for Syn {
    fn arbitrary(_u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        Ok(Self)
    }
}

impl<'a> Arbitrary<'a> for Ack {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        Ok(Self {
            cursors: u.arbitrary()?,
            epoch: u.arbitrary()?,
        })
    }
}

impl<'a> Arbitrary<'a> for Get {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        Ok(Self::new(Bin::arbitrary(u)?, u.arbitrary()?))
    }
}

impl<'a> Arbitrary<'a> for ChunkDescriptor {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        Ok(Self::new(
            ChunkAddress::arbitrary(u)?,
            BatchId::arbitrary(u)?,
            B256::arbitrary(u)?,
        ))
    }
}

impl<'a> Arbitrary<'a> for Offer {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        Ok(Self::new(u.arbitrary()?, u.arbitrary()?))
    }
}

impl<'a> Arbitrary<'a> for Want {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        Ok(Self::new(BitVector::from_wire_bytes(u.arbitrary()?)))
    }
}

impl<'a> Arbitrary<'a> for Delivery {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        let (chunk, _batch) =
            nectar_postage::generators::signed_stamped_chunk::<DEFAULT_BODY_SIZE>(u)?;
        Ok(Self::new(chunk))
    }
}
