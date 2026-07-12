//! Feature-gated [`arbitrary::Arbitrary`] impls for the retrieval wire types.
//!
//! Valid by construction: a delivery is the failure sentinel, a stampless
//! chunk (the shape the serve path emits), or a really signed stamped chunk
//! from nectar's valid-tier generators, so the proptest suite and the fuzz
//! round-trip target drive one construction path.

use arbitrary::{Arbitrary, Unstructured};
use nectar_primitives::{ChunkAddress, DEFAULT_BODY_SIZE, generators};

use crate::codec::{Delivery, Request};

impl<'a> Arbitrary<'a> for Request {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        Ok(Self::new(ChunkAddress::arbitrary(u)?))
    }
}

impl<'a> Arbitrary<'a> for Delivery {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        Ok(match u.int_in_range(0u8..=2)? {
            0 => Self::error(),
            1 => Self::chunk(generators::any_chunk(u)?, None),
            _ => {
                let (stamped, _batch) =
                    nectar_postage::generators::signed_stamped_chunk::<DEFAULT_BODY_SIZE>(u)?;
                Self::success(stamped)
            }
        })
    }
}
