//! Feature-gated [`arbitrary::Arbitrary`] impls for the pushsync wire types.
//!
//! Valid by construction: a delivery carries a really signed stamped chunk
//! from nectar's valid-tier generators, and a stored receipt carries a real
//! signature over the chunk address (the bytes a storer signs), so the
//! proptest suite and the fuzz round-trip target drive one construction path.

use alloy_signer::SignerSync;
use arbitrary::{Arbitrary, Unstructured};
use nectar_primitives::{ChunkAddress, DEFAULT_BODY_SIZE, Nonce};
use vertex_swarm_primitives::StorageRadius;

use crate::codec::{Delivery, ReceiptResponse, WireReceipt};

impl<'a> Arbitrary<'a> for Delivery {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        let (chunk, _batch) =
            nectar_postage::generators::signed_stamped_chunk::<DEFAULT_BODY_SIZE>(u)?;
        Ok(Self::new(chunk))
    }
}

impl<'a> Arbitrary<'a> for WireReceipt {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        let address = ChunkAddress::arbitrary(u)?;
        let signer = nectar_primitives::generators::signer(u)?;
        let signature = signer
            .sign_message_sync(address.as_bytes())
            .map_err(|_| arbitrary::Error::IncorrectFormat)?;
        Ok(Self::new(
            address,
            signature,
            Nonce::arbitrary(u)?,
            StorageRadius::arbitrary(u)?,
        ))
    }
}

impl<'a> Arbitrary<'a> for ReceiptResponse {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        if u.arbitrary()? {
            Ok(Self::Stored(WireReceipt::arbitrary(u)?))
        } else {
            Ok(Self::Failed)
        }
    }
}
