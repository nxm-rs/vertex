//! Feature-gated [`arbitrary::Arbitrary`] impls for the pseudosettle
//! messages, the shared construction path for the proptest suite and the
//! fuzz round-trip target.

use alloy_primitives::U256;
use arbitrary::{Arbitrary, Unstructured};

use crate::codec::{Payment, PaymentAck};

impl<'a> Arbitrary<'a> for Payment {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        Ok(Self::new(U256::from_be_bytes(u.arbitrary::<[u8; 32]>()?)))
    }
}

impl<'a> Arbitrary<'a> for PaymentAck {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        Ok(Self::new(
            U256::from_be_bytes(u.arbitrary::<[u8; 32]>()?),
            u.arbitrary()?,
        ))
    }
}
