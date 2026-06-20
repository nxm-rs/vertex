//! Synchronous validate-on-ingest admission for stamped chunks.
//!
//! The reserve's [`put`](vertex_swarm_api::SwarmLocalStore::put) is synchronous,
//! so admission runs the per-stamp checks against an already-loaded [`Batch`]
//! inside the reserve's own write transaction without re-reading the store. Each
//! stamp is validated on its own, so the validator holds no state beyond the
//! confirmation threshold.

use nectar_postage::{Batch, PostageContext, Stamp, StampError};
use nectar_primitives::ChunkAddress;

use crate::BatchId;

/// Why a stamped chunk was refused admission to the reserve.
#[derive(Debug, thiserror::Error)]
pub enum AdmissionError {
    #[error("postage batch {0} is unknown to the node")]
    UnknownBatch(BatchId),

    /// The batch exists but is not yet usable (too few confirmations).
    #[error("postage batch is not yet usable")]
    BatchNotUsable,

    /// The batch's value no longer covers the cumulative payout.
    #[error("postage batch has expired")]
    BatchExpired,

    #[error("stamp signer does not match the batch owner")]
    OwnerMismatch,

    #[error("stamp validation failed: {0}")]
    Stamp(#[from] StampError),
}

/// Synchronous, validate-on-ingest admission validator for stamped chunks.
///
/// The caller loads the batch before calling [`validate`](Self::validate), so the
/// validator carries no store dependency.
#[derive(Debug)]
pub struct AdmissionValidator {
    confirmation_threshold: u64,
}

impl AdmissionValidator {
    pub const fn new(confirmation_threshold: u64) -> Self {
        Self {
            confirmation_threshold,
        }
    }

    pub const fn confirmation_threshold(&self) -> u64 {
        self.confirmation_threshold
    }

    /// Validate a stamped chunk for admission.
    ///
    /// Checks, in order: batch usability and expiry against `context`, the
    /// structural index/bucket checks, and the signature (recover the signer and
    /// compare to the batch owner). `batch` must be the batch the stamp
    /// references (`stamp.batch() == batch.id()`).
    pub fn validate(
        &self,
        stamp: &Stamp,
        address: &ChunkAddress,
        batch: &Batch,
        context: &PostageContext,
    ) -> Result<(), AdmissionError> {
        if !batch.is_usable(context.block(), self.confirmation_threshold) {
            return Err(AdmissionError::BatchNotUsable);
        }
        if batch.is_expired(context.total_amount()) {
            return Err(AdmissionError::BatchExpired);
        }

        batch.validate_index(&stamp.stamp_index())?;
        batch.validate_bucket(&stamp.stamp_index(), address)?;

        // Map an owner mismatch to the precise refusal reason.
        match stamp.verify(address, batch.owner()) {
            Ok(()) => Ok(()),
            Err(StampError::OwnerMismatch { .. }) => Err(AdmissionError::OwnerMismatch),
            Err(e) => Err(AdmissionError::Stamp(e)),
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test assertions over known-bounds fixtures"
)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256};
    use alloy_signer::SignerSync;
    use alloy_signer_local::PrivateKeySigner;
    use nectar_postage::{StampDigest, current_timestamp};
    use nectar_primitives::{Chunk, DefaultContentChunk as ContentChunk};

    const THRESHOLD: u64 = 8;

    fn signer() -> PrivateKeySigner {
        PrivateKeySigner::from_bytes(&B256::repeat_byte(0x42)).expect("valid signer")
    }

    fn content_address() -> ChunkAddress {
        *ContentChunk::new(b"admission test payload".to_vec())
            .expect("valid content chunk")
            .address()
    }

    /// Batch owned by `owner`, depth 18 / bucket depth 16, with ample value so it
    /// is not expired against a zero cumulative payout.
    fn batch_for(owner: Address) -> Batch {
        Batch::new(B256::repeat_byte(0x11), 1_000_000, 0, owner, 18, 16, false)
    }

    /// Sign a stamp for `address` under `batch`, using a bucket/index consistent
    /// with the address so `validate_bucket` passes.
    fn signed_stamp(
        signer: &PrivateKeySigner,
        batch: &Batch,
        address: &ChunkAddress,
        timestamp: u64,
    ) -> Stamp {
        let bucket = batch.bucket_for_address(address);
        let index = nectar_postage::StampIndex::new(bucket, 0);
        let digest = StampDigest::new(*address, batch.id(), index, timestamp);
        let sig = signer
            .sign_hash_sync(&alloy_primitives::eip191_hash_message(
                digest.to_prehash().as_slice(),
            ))
            .expect("sign");
        Stamp::with_index(batch.id(), index, timestamp, sig)
    }

    #[test]
    fn admits_a_well_formed_stamp() {
        let s = signer();
        let owner = s.address();
        let batch = batch_for(owner);
        let addr = content_address();
        let stamp = signed_stamp(&s, &batch, &addr, current_timestamp());

        let v = AdmissionValidator::new(THRESHOLD);
        let ctx = PostageContext::new(THRESHOLD + 1, 0);
        v.validate(&stamp, &addr, &batch, &ctx).expect("admit");

        // A second stamp of the same batch validates independently: no per-batch
        // state between calls.
        let addr2 = *ContentChunk::new(b"second payload".to_vec())
            .unwrap()
            .address();
        let stamp2 = signed_stamp(&s, &batch, &addr2, current_timestamp());
        v.validate(&stamp2, &addr2, &batch, &ctx)
            .expect("admit second");
    }

    #[test]
    fn rejects_a_foreign_signer() {
        let s = signer();
        let batch = batch_for(s.address());
        let addr = content_address();

        // Different key: the recovered owner will not match.
        let other = PrivateKeySigner::from_bytes(&B256::repeat_byte(0x99)).unwrap();
        let stamp = signed_stamp(&other, &batch, &addr, current_timestamp());

        let v = AdmissionValidator::new(THRESHOLD);
        let ctx = PostageContext::new(THRESHOLD + 1, 0);
        let err = v.validate(&stamp, &addr, &batch, &ctx).unwrap_err();
        assert!(matches!(err, AdmissionError::OwnerMismatch));
    }

    #[test]
    fn rejects_an_unusable_then_an_expired_batch() {
        let s = signer();
        let batch = batch_for(s.address());
        let addr = content_address();
        let stamp = signed_stamp(&s, &batch, &addr, current_timestamp());
        let v = AdmissionValidator::new(THRESHOLD);

        // Block below the threshold: not usable.
        let cold = PostageContext::new(1, 0);
        assert!(matches!(
            v.validate(&stamp, &addr, &batch, &cold).unwrap_err(),
            AdmissionError::BatchNotUsable
        ));

        // Payout exceeds the batch value: expired.
        let expired = PostageContext::new(THRESHOLD + 1, u128::MAX);
        assert!(matches!(
            v.validate(&stamp, &addr, &batch, &expired).unwrap_err(),
            AdmissionError::BatchExpired
        ));
    }
}
