//! Feature-gated [`arbitrary::Arbitrary`] impl for the headers envelope.
//!
//! Keys are unique by map construction, so generated values round-trip
//! exactly through the wire encoding; the proptest suite and the fuzz
//! round-trip target drive one construction path.

use std::collections::HashMap;

use arbitrary::{Arbitrary, Unstructured};
use bytes::Bytes;

use crate::codec::Headers;

impl<'a> Arbitrary<'a> for Headers {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        let mut inner = HashMap::new();
        for entry in u.arbitrary_iter::<(String, Vec<u8>)>()? {
            let (key, value) = entry?;
            inner.insert(key, Bytes::from(value));
        }
        Ok(Self::new(inner))
    }
}
