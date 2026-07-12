//! Feature-gated [`arbitrary::Arbitrary`] impls for the swap messages, the
//! shared construction path for the proptest suite and the fuzz round-trip
//! target.
//!
//! The signature is 65 arbitrary bytes: the codec carries it opaquely (the
//! JSON is transport-only), so validity is the chequebook layer's concern,
//! but the length must be exact for the wire shape to encode.

use alloy_primitives::{Address, U256};
use arbitrary::{Arbitrary, Unstructured};
use bytes::Bytes;
use vertex_swarm_accounting_chequebook::{Cheque, ChequeExt, SignedCheque};

use crate::codec::{EmitCheque, Handshake};

impl<'a> Arbitrary<'a> for EmitCheque {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        let cheque = Cheque::new(
            Address::from(u.arbitrary::<[u8; 20]>()?),
            Address::from(u.arbitrary::<[u8; 20]>()?),
            U256::from_be_bytes(u.arbitrary::<[u8; 32]>()?),
        );
        let signature = Bytes::copy_from_slice(&u.arbitrary::<[u8; 65]>()?);
        Ok(Self::new(SignedCheque::new(cheque, signature)))
    }
}

impl<'a> Arbitrary<'a> for Handshake {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        Ok(Self::new(Address::from(u.arbitrary::<[u8; 20]>()?)))
    }
}
