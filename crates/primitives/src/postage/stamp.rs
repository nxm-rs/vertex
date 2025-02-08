use std::marker::PhantomData;

use alloy::{
    primitives::{FixedBytes, Keccak256, PrimitiveSignature, SignatureError, B256},
    signers::Signer,
};
use bytes::{Bytes, BytesMut};
use nectar_primitives_traits::AuthProof;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::chunk::Chunk;

use super::{Batch, BatchId};

// Type alias for Result
pub type Result<T> = std::result::Result<T, PostageStampError>;

// Size of components in a `PostageStamp`
const BATCH_ID_SIZE: usize = std::mem::size_of::<B256>();
const BUCKET_INDEX_SIZE: usize = std::mem::size_of::<u32>();
const BUCKET_SLOT_SIZE: usize = std::mem::size_of::<u32>();
const TIMESTAMP_SIZE: usize = std::mem::size_of::<u64>();
const SIGNATURE_SIZE: usize = 65;
// Total size of a `PostageStamp`
const POSTAGE_STAMP_SIZE: usize =
    BATCH_ID_SIZE + BUCKET_INDEX_SIZE + BUCKET_SLOT_SIZE + TIMESTAMP_SIZE + SIGNATURE_SIZE;

// Captures errors during stamp operations
#[derive(Debug, Error)]
pub enum PostageStampError {
    #[error("Invalid field {0}")]
    Invalid(&'static str),
    #[error("Mismatch received {0} expected {1}")]
    Mismatch(&'static str, &'static str),
    #[error("Incorrect size, received {0} bytes, expected {1} bytes")]
    IncorrectSize(usize, usize),
    #[error("Decode error: {0}")]
    DecodeError(#[from] std::array::TryFromSliceError),
    #[error("Signature error: {0}")]
    SignatureError(#[from] SignatureError),
    #[error("Signer error: {0}")]
    SignerError(#[from] alloy::signers::Error),
}

// Helper methods for error creation
impl PostageStampError {
    /// Creates an `Invalid` error
    pub fn invalid(field: &'static str) -> Self {
        PostageStampError::Invalid(field)
    }

    /// Creates a `Mismatch` error
    pub fn mismatch(received: &'static str, expected: &'static str) -> Self {
        PostageStampError::Mismatch(received, expected)
    }

    /// Creates an `IncorrectSize` error
    pub fn incorrect_size(received: usize, expected: usize) -> Self {
        PostageStampError::IncorrectSize(received, expected)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostageStamp {
    batch_id: BatchId,
    bucket_index: u32,
    bucket_slot: u32,
    timestamp: u64,
    signature: PrimitiveSignature,
}

impl PostageStamp {
    /// Creates a new `PostageStamp`
    pub fn new(
        batch: Batch,
        bucket_index: u32,
        bucket_slot: u32,
        timestamp: u64,
        signature: PrimitiveSignature,
    ) -> Self {
        PostageStamp {
            batch_id: *batch.id(),
            bucket_index,
            bucket_slot,
            timestamp,
            signature,
        }
    }

    /// Returns the `batch_id` of the `PostageStamp`
    pub fn batch_id(&self) -> &BatchId {
        &self.batch_id
    }

    /// Returns the `bucket_index` of the `PostageStamp`
    pub fn bucket_index(&self) -> u32 {
        self.bucket_index
    }

    /// Returns the `bucket_slot` of the `PostageStamp`
    pub fn bucket_slot(&self) -> u32 {
        self.bucket_slot
    }

    /// Returns the `timestamp` of the `PostageStamp`
    pub fn timestamp(&self) -> u64 {
        self.timestamp
    }

    /// Returns the `signature` of the `PostageStamp`
    pub fn signature(&self) -> &PrimitiveSignature {
        &self.signature
    }

    /// Returns the hash of the stamp
    pub fn hash(&self) -> FixedBytes<32> {
        let mut hasher = Keccak256::new();
        hasher.update(&self.batch_id);
        hasher.update(&self.bucket_index.to_be_bytes());
        hasher.update(&self.bucket_slot.to_be_bytes());
        hasher.update(&self.timestamp.to_be_bytes());
        hasher.update(&self.signature.as_bytes());

        hasher.finalize()
    }
}

impl From<PostageStamp> for Bytes {
    fn from(stamp: PostageStamp) -> Self {
        // Serializes the `PostageStamp` into a byte array
        let mut buf = BytesMut::with_capacity(POSTAGE_STAMP_SIZE);
        buf.extend_from_slice(stamp.batch_id().as_ref());
        buf.extend_from_slice(stamp.bucket_index().to_be_bytes().as_ref());
        buf.extend_from_slice(stamp.bucket_slot().to_be_bytes().as_ref());
        buf.extend_from_slice(stamp.timestamp.to_be_bytes().as_ref());
        buf.extend_from_slice(stamp.signature.as_bytes().as_ref());

        Bytes::from(buf.freeze())
    }
}

impl TryFrom<Bytes> for PostageStamp {
    type Error = PostageStampError;

    fn try_from(mut bytes: Bytes) -> Result<Self> {
        // Deserializes a byte array into a `PostageStamp`
        if bytes.len() != POSTAGE_STAMP_SIZE {
            return Err(PostageStampError::incorrect_size(
                bytes.len(),
                POSTAGE_STAMP_SIZE,
            ));
        }

        let batch_id = BatchId::from_slice(&bytes.split_to(BATCH_ID_SIZE));
        let bucket_index =
            u32::from_be_bytes(bytes.split_to(BUCKET_INDEX_SIZE).as_ref().try_into()?);
        let bucket_slot = u32::from_be_bytes(bytes.split_to(BUCKET_SLOT_SIZE).as_ref().try_into()?);
        let timestamp = u64::from_be_bytes(bytes.split_to(TIMESTAMP_SIZE).as_ref().try_into()?);
        let signature = PrimitiveSignature::try_from(bytes.as_ref())?;

        Ok(PostageStamp {
            batch_id,
            bucket_index,
            bucket_slot,
            timestamp,
            signature,
        })
    }
}

impl TryFrom<&[u8]> for PostageStamp {
    type Error = PostageStampError;

    fn try_from(buf: &[u8]) -> Result<Self> {
        Self::try_from(Bytes::copy_from_slice(buf))
    }
}

impl AuthProof for PostageStamp {
    fn proof_data(&self) -> Bytes {
        self.clone().into()
    }
}

// Configuration container for building a `PostageStamp`.
#[derive(Default)]
struct StampConfig {
    chunk: Option<Chunk>,
    batch_id: Option<BatchId>,
    bucket_index: Option<u32>,
    bucket_slot: Option<u32>,
    timestamp: Option<u64>,
    signature: Option<PrimitiveSignature>,
}

impl StampConfig {
    /// Builds the `PostageStamp` from the configured parameters.
    fn build(self) -> PostageStamp {
        PostageStamp {
            batch_id: self.batch_id.expect("Missing batch ID"),
            bucket_index: self.bucket_index.expect("Missing bucket index"),
            bucket_slot: self.bucket_slot.expect("Missing bucket slot"),
            timestamp: self.timestamp.expect("Missing timestamp"),
            signature: self.signature.expect("Missing signature"),
        }
    }
}

/// A stateful builder for creating postage stamps.
pub struct StampBuilder<S> {
    config: StampConfig,
    _state: PhantomData<S>,
}

impl<S> StampBuilder<S> {
    /// Sets the bucket index and returns a new builder instance.
    pub fn with_bucket_index(mut self, index: u32) -> Self {
        self.config.bucket_index = Some(index);
        self
    }

    /// Sets the bucket slot and returns a new builder instance.
    pub fn with_bucket_slot(mut self, slot: u32) -> Self {
        self.config.bucket_slot = Some(slot);
        self
    }

    /// Sets the timestamp and returns a new builder instance.
    pub fn with_timestamp(mut self, timestamp: u64) -> Self {
        self.config.timestamp = Some(timestamp);
        self
    }
}

pub trait BuilderState {}

// Initial state of the stamp builder before any fields are set.
pub struct Initial;
impl BuilderState for Initial {}

// State of the stamp builder after a batch has been set.
pub struct WithBatchId;
impl BuilderState for WithBatchId {}

// State of the stamp builder after all required parameters have been set.
pub struct ReadyToBuild;

impl StampBuilder<Initial> {
    /// Creates a new stamp builder in the initial state.
    pub fn new() -> Self {
        Self {
            config: StampConfig::default(),
            _state: PhantomData,
        }
    }

    /// Sets the batch and transitions to `WithBatchId` state.
    pub fn with_batch(mut self, batch: &Batch) -> StampBuilder<WithBatchId> {
        self.config.batch_id = Some(*batch.id());
        StampBuilder {
            config: self.config,
            _state: PhantomData,
        }
    }

    /// Sets the batch id and transitions to `WithBatchId` state.
    pub fn with_batch_id(mut self, batch_id: BatchId) -> StampBuilder<WithBatchId> {
        self.config.batch_id = Some(batch_id);
        StampBuilder {
            config: self.config,
            _state: PhantomData,
        }
    }
}

impl StampBuilder<WithBatchId> {
    /// Manually sets the signature and transitions to `ReadyToBuild` state.
    pub fn with_signature(mut self, signature: PrimitiveSignature) -> StampBuilder<ReadyToBuild> {
        self.config.signature = Some(signature);
        StampBuilder {
            config: self.config,
            _state: PhantomData,
        }
    }

    /// Sets the signer and derives the signature, then transitions to `ReadyToBuild` state.
    pub async fn with_signer(
        self,
        signer: impl Signer + Send + Sync,
    ) -> Result<StampBuilder<ReadyToBuild>> {
        // Here we assume that a method `sign_message` exists for the Signer trait
        // which can be used to sign a message and return a signature.
        let message = self.config.batch_id.expect("Batch ID must be set");
        let signature = signer.sign_message(message.as_ref()).await?;

        Ok(self.with_signature(signature))
    }
}

impl StampBuilder<ReadyToBuild> {
    /// Builds the `PostageStamp` from the configured parameters.
    pub fn build(self) -> PostageStamp {
        self.config.build()
    }
}
