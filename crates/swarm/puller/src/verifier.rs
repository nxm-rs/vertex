//! [`PullChunkVerifier`] implementations.
//!
//! [`SignatureVerifier`] is the interim signature-only gate: it proves the stamp
//! signature is well-formed and bound to the chunk address but does not consult
//! the batch set. [`FundingVerifier`] is the full check: it loads the stamp's
//! batch from a [`BatchStore`] and runs the same usability, expiry, capacity and
//! owner checks the reserve enforces on admission, so an underfunded or expired
//! batch taints the syncing page rather than being silently dropped at the
//! reserve put.

use nectar_postage::{BatchStore, StampError};
use vertex_swarm_api::{PullChunkVerifier, VerifyError};
use vertex_swarm_postage::{AdmissionError, AdmissionValidator};
use vertex_swarm_primitives::StampedChunk;

/// Signature-only verifier: recovers the stamp signer over the chunk address.
#[derive(Debug, Clone, Copy, Default)]
pub struct SignatureVerifier;

impl PullChunkVerifier for SignatureVerifier {
    fn verify(&self, chunk: &StampedChunk) -> Result<(), VerifyError> {
        chunk
            .stamp()
            .recover_signer(chunk.address())
            .map(|_| ())
            .map_err(|_| VerifyError::InvalidSignature)
    }
}

/// Full admission verifier: signature recovery plus on-chain batch funding.
///
/// Resolves the stamp's batch from `batches` (the chain-indexed postage set) and
/// runs [`AdmissionValidator`], so a chunk stamped under an unknown, unconfirmed,
/// expired or over-capacity batch, or signed by a non-owner, is rejected before
/// it reaches the reserve. An unknown batch is the common case during catch-up,
/// not an error.
#[derive(Debug)]
pub struct FundingVerifier<BS> {
    batches: BS,
    admission: AdmissionValidator,
}

impl<BS> FundingVerifier<BS> {
    pub const fn new(batches: BS, admission: AdmissionValidator) -> Self {
        Self { batches, admission }
    }
}

impl<BS> PullChunkVerifier for FundingVerifier<BS>
where
    BS: BatchStore + Send + Sync,
    BS::Error: Send + Sync,
{
    fn verify(&self, chunk: &StampedChunk) -> Result<(), VerifyError> {
        let stamp = chunk.stamp();
        let address = chunk.address();

        let batch = self
            .batches
            .get(&stamp.batch())
            .map_err(|_| VerifyError::Malformed)?
            .ok_or(VerifyError::UnknownBatch)?;
        let context = self.batches.context().map_err(|_| VerifyError::Malformed)?;

        self.admission
            .validate(stamp, address, &batch, &context)
            .map_err(map_admission_error)
    }
}

/// Map the reserve's admission taxonomy onto the puller's verify taxonomy.
///
/// Unusable and expired batches collapse onto `UnknownBatch` (the verifier's
/// "not currently a fundable batch" outcome); an index or bucket overflow is the
/// batch lacking capacity at the stamp's slot; a foreign signer is an invalid
/// signature; everything else is structural.
fn map_admission_error(err: AdmissionError) -> VerifyError {
    match err {
        AdmissionError::UnknownBatch(_)
        | AdmissionError::BatchNotUsable
        | AdmissionError::BatchExpired => VerifyError::UnknownBatch,
        AdmissionError::OwnerMismatch => VerifyError::InvalidSignature,
        AdmissionError::Stamp(StampError::InvalidIndex | StampError::BucketFull { .. }) => {
            VerifyError::InsufficientFunding
        }
        AdmissionError::Stamp(StampError::InvalidSignature) => VerifyError::InvalidSignature,
        AdmissionError::Stamp(_) => VerifyError::Malformed,
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
    use nectar_postage::{
        Batch, BatchId, PostageContext, Stamp, StampDigest, StampIndex, current_timestamp,
    };
    use nectar_primitives::{Chunk, ContentChunk};
    use std::collections::HashMap;
    use std::convert::Infallible;
    use vertex_swarm_primitives::StampedChunk;

    const THRESHOLD: u64 = 8;

    /// A stamp whose signature does not recover to a valid signer (zeroed sig).
    fn unsigned_chunk() -> StampedChunk {
        let chunk = ContentChunk::new(&b"verify-test"[..]).unwrap();
        let mut raw = [0u8; 65];
        raw[64] = 27;
        let sig = alloy_primitives::Signature::try_from(&raw[..]).unwrap();
        let stamp = Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, sig);
        StampedChunk::new(chunk.into(), stamp)
    }

    #[test]
    fn malformed_signature_is_rejected() {
        // A zero signature recovers no signer; the verifier rejects it.
        let err = SignatureVerifier.verify(&unsigned_chunk()).unwrap_err();
        assert!(matches!(err, VerifyError::InvalidSignature));
    }

    /// In-memory `BatchStore` exposing only the two methods the funding verifier
    /// reads; the rest are unreachable in these tests.
    #[derive(Default)]
    struct MockBatchStore {
        batches: HashMap<BatchId, Batch>,
        context: PostageContext,
    }

    impl MockBatchStore {
        fn with_batch(mut self, batch: Batch) -> Self {
            self.batches.insert(batch.id(), batch);
            self
        }

        fn with_context(mut self, context: PostageContext) -> Self {
            self.context = context;
            self
        }
    }

    impl BatchStore for MockBatchStore {
        type Error = Infallible;

        fn get(&self, id: &BatchId) -> Result<Option<Batch>, Self::Error> {
            Ok(self.batches.get(id).cloned())
        }

        fn put(&self, _batch: Batch) -> Result<(), Self::Error> {
            unreachable!("verifier never writes")
        }

        fn remove(&self, _id: &BatchId) -> Result<bool, Self::Error> {
            unreachable!("verifier never removes")
        }

        fn contains(&self, _id: &BatchId) -> Result<bool, Self::Error> {
            unreachable!("verifier never probes presence")
        }

        fn context(&self) -> Result<PostageContext, Self::Error> {
            Ok(self.context)
        }

        fn set_context(&self, _state: PostageContext) -> Result<(), Self::Error> {
            unreachable!("verifier never writes the context")
        }

        fn batch_ids(&self) -> Result<Vec<BatchId>, Self::Error> {
            unreachable!("verifier never enumerates")
        }

        fn count(&self) -> Result<usize, Self::Error> {
            unreachable!("verifier never counts")
        }
    }

    fn signer() -> PrivateKeySigner {
        PrivateKeySigner::from_bytes(&B256::repeat_byte(0x42)).expect("valid signer")
    }

    fn content_chunk() -> ContentChunk {
        ContentChunk::new(b"funding verifier payload".to_vec()).expect("valid content chunk")
    }

    /// Batch owned by `owner`, depth 18 / bucket depth 16, with `value` funding.
    fn batch_for(owner: Address, value: u128) -> Batch {
        Batch::new(B256::repeat_byte(0x11), value, 0, owner, 18, 16, false)
    }

    /// Build a stamped chunk whose stamp is signed for `chunk` under `batch`,
    /// using `index` so capacity behaviour can be exercised.
    fn signed_chunk(
        s: &PrivateKeySigner,
        batch: &Batch,
        chunk: &ContentChunk,
        index: u32,
    ) -> StampedChunk {
        let address = chunk.address();
        let bucket = batch.bucket_for_address(address);
        let stamp_index = StampIndex::new(bucket, index);
        let timestamp = current_timestamp();
        let digest = StampDigest::new(*address, batch.id(), stamp_index, timestamp);
        let sig = s
            .sign_hash_sync(&alloy_primitives::eip191_hash_message(
                digest.to_prehash().as_slice(),
            ))
            .expect("sign");
        let stamp = Stamp::with_index(batch.id(), stamp_index, timestamp, sig);
        StampedChunk::new(chunk.clone().into(), stamp)
    }

    fn verifier(store: MockBatchStore) -> FundingVerifier<MockBatchStore> {
        FundingVerifier::new(store, AdmissionValidator::new(THRESHOLD))
    }

    /// A context with enough confirmations and no consumed payout.
    fn usable_context() -> PostageContext {
        PostageContext::new(THRESHOLD + 1, 0)
    }

    #[test]
    fn funded_batch_admits() {
        let s = signer();
        let batch = batch_for(s.address(), 1_000_000);
        let chunk = content_chunk();
        let stamped = signed_chunk(&s, &batch, &chunk, 0);

        let store = MockBatchStore::default()
            .with_batch(batch)
            .with_context(usable_context());
        verifier(store)
            .verify(&stamped)
            .expect("funded chunk admits");
    }

    #[test]
    fn unknown_batch_is_rejected() {
        let s = signer();
        // The batch is never inserted into the store.
        let batch = batch_for(s.address(), 1_000_000);
        let chunk = content_chunk();
        let stamped = signed_chunk(&s, &batch, &chunk, 0);

        let store = MockBatchStore::default().with_context(usable_context());
        let err = verifier(store).verify(&stamped).unwrap_err();
        assert!(matches!(err, VerifyError::UnknownBatch));
    }

    #[test]
    fn expired_batch_is_rejected() {
        let s = signer();
        let batch = batch_for(s.address(), 1_000);
        let chunk = content_chunk();
        let stamped = signed_chunk(&s, &batch, &chunk, 0);

        // Consumed payout exceeds the batch value: expired.
        let store = MockBatchStore::default()
            .with_batch(batch)
            .with_context(PostageContext::new(THRESHOLD + 1, u128::MAX));
        let err = verifier(store).verify(&stamped).unwrap_err();
        assert!(matches!(err, VerifyError::UnknownBatch));
    }

    #[test]
    fn unconfirmed_batch_is_rejected() {
        let s = signer();
        let batch = batch_for(s.address(), 1_000_000);
        let chunk = content_chunk();
        let stamped = signed_chunk(&s, &batch, &chunk, 0);

        // Block below the confirmation threshold: not yet usable.
        let store = MockBatchStore::default()
            .with_batch(batch)
            .with_context(PostageContext::new(1, 0));
        let err = verifier(store).verify(&stamped).unwrap_err();
        assert!(matches!(err, VerifyError::UnknownBatch));
    }

    #[test]
    fn over_capacity_index_is_insufficient_funding() {
        let s = signer();
        let batch = batch_for(s.address(), 1_000_000);
        let chunk = content_chunk();
        // depth 18, bucket depth 16: per-bucket capacity is 2^(18-16) = 4, so
        // index 4 exceeds the batch's capacity at this slot.
        let stamped = signed_chunk(&s, &batch, &chunk, 4);

        let store = MockBatchStore::default()
            .with_batch(batch)
            .with_context(usable_context());
        let err = verifier(store).verify(&stamped).unwrap_err();
        assert!(matches!(err, VerifyError::InsufficientFunding));
    }

    #[test]
    fn foreign_signer_is_invalid_signature() {
        let s = signer();
        let owner = PrivateKeySigner::from_bytes(&B256::repeat_byte(0x99)).unwrap();
        // Batch owned by `owner`, but the stamp is signed by `s`.
        let batch = batch_for(owner.address(), 1_000_000);
        let chunk = content_chunk();
        let stamped = signed_chunk(&s, &batch, &chunk, 0);

        let store = MockBatchStore::default()
            .with_batch(batch)
            .with_context(usable_context());
        let err = verifier(store).verify(&stamped).unwrap_err();
        assert!(matches!(err, VerifyError::InvalidSignature));
    }
}
