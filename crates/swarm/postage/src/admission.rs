//! Synchronous validate-on-ingest admission for stamped chunks.
//!
//! The reserve's [`put`](vertex_swarm_api::SwarmLocalStore::put) is synchronous
//! (the storage lattice trait is sync, redb is sync), so admission must be a
//! synchronous decision taken inside the same call. The nectar
//! [`StoreValidator`](nectar_postage::StoreValidator) expresses the canonical
//! checks by loading the batch through a
//! [`BatchStore`](nectar_postage::BatchStore) and applying them. This module is
//! the seam that performs *exactly* the same checks against an already-loaded
//! [`Batch`], so the reserve can validate on ingest inside its own write
//! transaction without re-reading the store.
//!
//! The validator is **stateless**: each stamp is validated on its own, by
//! recovering the signer and comparing it to the batch owner. It holds no
//! per-batch key cache and no sidecar table; the only state it carries is the
//! confirmation threshold policy.
//!
//! # What is reused (and what is not re-implemented)
//!
//! The per-stamp checks are nectar's, mirroring
//! [`StoreValidator::validate`](nectar_postage::StoreValidator) against an
//! already-loaded batch:
//!
//! - the batch is usable (enough confirmations): [`Batch::is_usable`];
//! - the batch is not expired: [`Batch::is_expired`];
//! - the stamp index is in bounds: [`Batch::validate_index`];
//! - the bucket matches the address: [`Batch::validate_bucket`];
//! - the signature recovers the batch owner: [`Stamp::verify`] (the EIP-191
//!   ecrecover, byte-equal to bee).
//!
//! The digest and ecrecover are never re-implemented here: they live in nectar
//! and are reached only through [`Stamp`]'s own methods.
//!
//! # Deferred optimisation: per-process signer memoisation
//!
//! ecrecover is the dominant cost of stamp validation, and a batch's stamps all
//! share one signer, so the recover-once / [`Stamp::verify_with_pubkey`]-the-rest
//! pattern (nectar-supported) is a legitimate future optimisation. It is
//! deliberately **not** implemented here. If profiling justifies it, it must be
//! a per-process in-memory memoisation of the recovered key, gated behind the
//! batch-existence check and matched to the batch owner before it is trusted.
//! It must **never** be a persisted sidecar table: a batch's signer is derivable
//! from any one of its stamps, so persisting it adds a consensus-irrelevant
//! table that can drift from the canonical batch store. Until then the validator
//! recovers per stamp, which keeps validation provably stateless.

use nectar_postage::{Batch, PostageContext, Stamp, StampError};
use nectar_primitives::ChunkAddress;

use crate::BatchId;

/// Why a stamped chunk was refused admission to the reserve.
#[derive(Debug, thiserror::Error)]
pub enum AdmissionError {
    /// The referenced batch is not known to the node (the create was missed or
    /// the batch never existed).
    #[error("postage batch {0} is unknown to the node")]
    UnknownBatch(BatchId),

    /// The batch exists but is not yet usable (too few confirmations).
    #[error("postage batch is not yet usable")]
    BatchNotUsable,

    /// The batch has expired (its value no longer covers the cumulative payout).
    #[error("postage batch has expired")]
    BatchExpired,

    /// The recovered signer does not match the batch owner.
    #[error("stamp signer does not match the batch owner")]
    OwnerMismatch,

    /// A structural or cryptographic stamp failure surfaced by nectar.
    #[error("stamp validation failed: {0}")]
    Stamp(#[from] StampError),
}

/// A synchronous, validate-on-ingest admission validator for stamped chunks.
///
/// Stateless apart from the confirmation threshold the node enforces. Construct
/// with that threshold; call [`validate`](Self::validate) on the ingest path
/// with the stamp, the chunk address, the already-loaded [`Batch`], and the
/// current [`PostageContext`]. The validator performs the nectar checks
/// synchronously and returns `Ok(())` only when the stamp is admissible.
///
/// The batch is loaded by the caller (the reserve holds a
/// [`BatchStore`](nectar_postage::BatchStore) and reads it before entering its
/// write transaction); passing it in keeps this validator free of any store
/// dependency and lets it run inside a synchronous redb transaction.
#[derive(Debug)]
pub struct AdmissionValidator {
    /// Minimum block confirmations for a batch to be usable.
    confirmation_threshold: u64,
}

impl AdmissionValidator {
    /// Create an admission validator enforcing `confirmation_threshold` block
    /// confirmations before a batch is usable.
    pub const fn new(confirmation_threshold: u64) -> Self {
        Self {
            confirmation_threshold,
        }
    }

    /// The confirmation threshold this validator enforces.
    pub const fn confirmation_threshold(&self) -> u64 {
        self.confirmation_threshold
    }

    /// Validate a stamped chunk for admission, synchronously.
    ///
    /// Performs, in order: batch usability and expiry against `context`, the
    /// structural index/bucket checks, and the signature check (recover the
    /// signer and compare to the batch owner). Returns `Ok(())` only when every
    /// check passes.
    ///
    /// `batch` must be the batch the stamp references (`stamp.batch() ==
    /// batch.id()`); the reserve loads it before calling. The owner match uses
    /// the batch's on-chain owner, so a stamp signed by anyone else is refused
    /// even if its structure is sound.
    pub fn validate(
        &self,
        stamp: &Stamp,
        address: &ChunkAddress,
        batch: &Batch,
        context: &PostageContext,
    ) -> Result<(), AdmissionError> {
        // Usability and expiry are policy over the live context, mirroring
        // nectar's StoreValidator::get_batch_for_stamp (which calls get_usable).
        if !batch.is_usable(context.block(), self.confirmation_threshold) {
            return Err(AdmissionError::BatchNotUsable);
        }
        if batch.is_expired(context.total_amount()) {
            return Err(AdmissionError::BatchExpired);
        }

        // Structural checks (nectar, called verbatim).
        batch.validate_index(&stamp.stamp_index())?;
        batch.validate_bucket(&stamp.stamp_index(), address)?;

        // Signature: recover the signer and compare to the batch owner. This is
        // nectar's Stamp::verify (ecrecover + owner equality), called verbatim;
        // it maps an owner mismatch to OwnerMismatch so the reserve reports the
        // precise refusal reason.
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

    /// A batch owned by `owner`, created at block 0, depth 18 / bucket depth 16,
    /// with ample value so it is not expired against a zero cumulative payout.
    fn batch_for(owner: Address) -> Batch {
        Batch::new(B256::repeat_byte(0x11), 1_000_000, 0, owner, 18, 16, false)
    }

    /// Sign a real stamp for `address` under `batch` at `timestamp`, using a
    /// bucket/index consistent with the address so validate_bucket passes.
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
        // Usable at a block well past the threshold, not expired (payout 0).
        let ctx = PostageContext::new(THRESHOLD + 1, 0);
        v.validate(&stamp, &addr, &batch, &ctx).expect("admit");

        // A second, distinct stamp of the same batch validates independently;
        // the validator carries no per-batch state between calls.
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

        // Sign with a different key: the recovered owner will not match.
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

        // Cumulative payout exceeds the batch value: expired.
        let expired = PostageContext::new(THRESHOLD + 1, u128::MAX);
        assert!(matches!(
            v.validate(&stamp, &addr, &batch, &expired).unwrap_err(),
            AdmissionError::BatchExpired
        ));
    }
}
