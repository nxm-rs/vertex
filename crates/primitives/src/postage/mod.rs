use alloy::{
    network::BlockResponse,
    primitives::{
        Address, BlockNumber, BlockTimestamp, FixedBytes, PrimitiveSignature, B256, U256,
    },
    signers::Signer,
};
use nectar_primitives_traits::{
    AuthProof, AuthProofGenerator, AuthResult, Authorizer, Chunk, ResourceBoundAuthorizer,
    TimeBoundAuthorizer,
};

pub type BatchId = FixedBytes<32>;
mod batch;
mod stamp;
// mod tracker;
pub use batch::*;
pub use stamp::*;
// pub use tracker::*;

// pub struct PostageAuthorizer {
//     batch_store: BatchStore,
//     chain_state: ChainState,
// }

// impl Authorizer for PostageAuthorizer {
//     type Proof = PostageStamp;

//     fn validate(&self, chunk: &impl Chunk, proof: &Self::Proof) -> AuthResult<()> {
//         // 1. Retrieve batch
//         let batch = self.batch_store.get(proof.batch_id)?;

//         // 2. Verify batch is valid
//         self.verify_batch_valid(&batch)?;

//         // 3. Verify stamp signature against batch owner
//         self.verify_signature(chunk, proof, &batch)?;

//         // 4. Verify bucket assignment and capacity
//         self.verify_bucket_capacity(chunk, proof, &batch)?;

//         Ok(())
//     }
// }

// impl TimeBoundAuthorizer for PostageAuthorizer {
//     fn cleanup_expired(&mut self, now: BlockTimestamp) -> AuthResult<u64> {
//         let mut removed = 0;

//         // Cleanup batches that are expired based on:
//         // - Chain state total amount exceeding batch value
//         // - Block number thresholds
//         // - Any other time-based expiry conditions

//         Ok(removed)
//     }
// }

// impl ResourceBoundAuthorizer for PostageAuthorizer {
//     fn total_capacity(&self) -> u64 {
//         // Sum of all valid batch capacities
//         self.batch_store
//             .valid_batches()
//             .map(|b| 2u64.pow(b.depth as u32))
//             .sum()
//     }

//     fn used_capacity(&self) -> u64 {
//         // Track usage across all batches
//         self.batch_store
//             .valid_batches()
//             .map(|b| b.utilized_capacity())
//             .sum()
//     }
// }

// /// Handles stamp creation within a batch
// pub struct PostageGenerator {
//     batch: Batch,
//     signer: Signer,
//     bucket_tracker: BucketTracker,
// }

// impl AuthProofGenerator for PostageGenerator {
//     type Proof = PostageStamp;

//     fn generate_proof(&self, chunk: &impl Chunk) -> AuthResult<Self::Proof> {
//         // 1. Check batch still has capacity
//         if !self.has_available_capacity() {
//             return Err(AuthError::CapacityExceeded);
//         }

//         // 2. Calculate bucket and index
//         let (bucket, index) = self.bucket_tracker.next_index(chunk)?;

//         // 3. Create and sign stamp
//         let stamp = PostageStamp {
//             batch_id: self.batch.id,
//             index: index.to_bytes(),
//             timestamp: now().to_bytes(),
//             signature: self.sign(chunk, index)?,
//         };

//         // 4. Update bucket usage
//         self.bucket_tracker.record_usage(bucket)?;

//         Ok(stamp)
//     }
// }

// struct BucketTracker {
//     buckets: Vec<u32>,
//     depth: u8,
//     bucket_depth: u8,
// }

// impl BucketTracker {
//     fn next_index(&self, chunk: &impl Chunk) -> AuthResult<(u32, u32)> {
//         // Calculate bucket and next available index
//         todo!()
//     }

//     fn record_usage(&mut self, bucket: u32) -> AuthResult<()> {
//         // Track usage in bucket
//         todo!()
//     }
// }

#[derive(Debug)]
pub struct ChainState {
    pub block_number: BlockNumber,
    pub block_timestamp: BlockTimestamp,
    pub payment: U256,
    pub price: U256,
}

// trait BatchStore {
//     fn get(&self, id: &[u8]) -> AuthResult<Batch>;
//     fn valid_batches(&self) -> impl Iterator<Item = &Batch>;
// }
