//! Feature-gated [`arbitrary::Arbitrary`] impl for the pricing message, the
//! shared construction path for the proptest suite and the fuzz round-trip
//! target.

use alloy_primitives::U256;
use arbitrary::{Arbitrary, Unstructured};

use crate::codec::AnnouncePaymentThreshold;

impl<'a> Arbitrary<'a> for AnnouncePaymentThreshold {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        Ok(Self::new(U256::from_be_bytes(u.arbitrary::<[u8; 32]>()?)))
    }
}
