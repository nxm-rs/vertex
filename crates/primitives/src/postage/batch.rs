//! The `postage::Batch` module provides data structures and utilities for managing batches of storage
//! capacity in a decentralised storage network context. A batch represents a unit of storage that can
//! be purchased, used, and eventually expires based on its usage and configuration.
//!
//! This module includes:
//!
//! - The `Batch` struct: represents a batch with properties like ID, value, owner, depth, etc
//! - The `BatchError` enum: Captures errors related to batch operations.
//! - A builder pattern (`BatchBuilder`): Facilitates the creation of batches by setting various
//!   parameters in a structured manner.
use alloy::{
    dyn_abi::DynSolValue,
    primitives::{Address, BlockNumber, BlockTimestamp, FixedBytes, Keccak256, B256, U256},
    signers::Signer,
};
use nectar_primitives_traits::CHUNK_SIZE;
use serde::{Deserialize, Serialize};
use std::marker::PhantomData;
use thiserror::Error;

pub type BatchId = FixedBytes<32>;

// Type alias for Result
pub type Result<T> = std::result::Result<T, BatchError>;

// TODO: Implement `encode` / `decode` for `Batch` for serde to/from KV storage.
/// Represents a storage unit with specific characteristics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Batch {
    /// The unique identifier of the batch, generally `H(nonce|owner)`.
    id: BatchId,
    /// The normalised balance associated with the batch, which represents a per-chunk value.
    /// If value is `None`, the batch is considered to not have been created on chain (yet).
    value: Option<U256>,
    /// the address that owns the batch and can sign stamps for it.
    owner: Address,
    /// Determines the number of chunks that can be signed by the batch. (2^depth)
    depth: u8,
    /// Specifies the uniformity of the batch, which is the number of bits that are used to determine
    /// the bucket in which a stamp is placed.
    bucket_depth: u8,
    /// Indicates if the batch is immutable.
    immutable: bool,
}

impl Batch {
    /// Returns the unique identifier of the batch.
    pub fn id(&self) -> &BatchId {
        &self.id
    }

    /// Determines whether the batch is immutable.
    pub fn immutable(&self) -> bool {
        self.immutable
    }

    /// Returns the bucket depth of the batch.
    pub(crate) fn bucket_depth(&self) -> u8 {
        self.bucket_depth
    }

    /// Calculates the remaining time-to-live (TTL) in blocks.
    ///
    /// If `value` <= `current_out_payment`, the batch is considered expired and the TTL is 0.
    pub fn ttl_blocks(&self, current_out_payment: U256, current_price: U256) -> u64 {
        if let Some(value) = self.value {
            if value > current_out_payment {
                return (value - current_out_payment / current_price).to();
            }
        }

        0
    }

    /// Converts the remaining TTL from blocks to seconds using the block time.
    pub const fn ttl_seconds(&self, blocks_remaining: u64, block_time: u64) -> u64 {
        blocks_remaining * block_time
    }

    /// Calculates the TTL in seconds based on current out payment, price, and block time.
    pub fn ttl(&self, current_out_payment: U256, current_price: U256, block_time: u64) -> u64 {
        self.ttl_seconds(
            self.ttl_blocks(current_out_payment, current_price),
            block_time,
        )
    }

    /// Determines the block number when the batch expires.
    pub fn expiry_block_number(
        &self,
        current_out_payment: U256,
        current_price: U256,
        current_block_number: BlockNumber,
    ) -> BlockNumber {
        current_block_number + self.ttl_blocks(current_out_payment, current_price)
    }

    /// Determines the expiration time of the batch in Unix timestamp.
    pub fn expiry(
        &self,
        current_out_payment: U256,
        current_price: U256,
        current_timestamp: BlockTimestamp,
        block_time: u64,
    ) -> u64 {
        current_timestamp
            + self.ttl_seconds(
                self.ttl_blocks(current_out_payment, current_price),
                block_time,
            )
    }

    /// Checks if the batch has expired based on the current block number.
    pub fn expired(
        &self,
        current_out_payment: U256,
        current_price: U256,
        current_block_number: BlockNumber,
    ) -> bool {
        self.expiry_block_number(current_out_payment, current_price, current_block_number)
            <= current_block_number
    }

    /// Returns the maximum number of collisions possible in a bucket.
    #[must_use]
    pub const fn max_collisions(&self) -> u64 {
        2_u64.pow((self.depth - self.bucket_depth) as u32)
    }

    /// Calculates the cost of a batch for the additional target duration and current storage price.
    #[must_use]
    pub fn cost(&self, duration_blocks: impl Into<Duration>, price: U256) -> Result<U256> {
        let chunks = Self::chunks(self.depth);

        Ok(U256::from(chunks) * price * U256::from(duration_blocks.into().to_blocks()?))
    }

    /// Returns the number of chunks in the batch (2^depth).
    ///
    /// # Panics
    /// If `depth` is greater than 63, this function will panic.
    #[track_caller]
    pub const fn chunks(depth: u8) -> u64 {
        2_u64.pow(depth as u32)
    }

    /// Returns the number of bytes that may be stored in the batch.
    pub const fn size(depth: u8) -> u64 {
        Self::chunks(depth) * CHUNK_SIZE as u64
    }

    /// Calculates the required depth for a given size in bytes.
    ///
    /// Note: Uploading data of 0 bytes is allowed by represents an empty chunk.
    #[must_use]
    pub fn depth_for_size(size: u64) -> u8 {
        // A minimum of 1 chunk is always required, irrespective of the size.
        let chunks = (size / CHUNK_SIZE as u64).max(1);
        (chunks as f64).log2().ceil() as u8
    }
}

/// Captures errors during batch operations.
#[derive(Debug, Error)]
pub enum BatchError {
    #[error("Invalid depth: {0}")]
    InvalidDepth(u8),
    #[error("Invalid bucket depth: {0}")]
    InvalidBucketDepth(u8),
    #[error("Size is too small for batch")]
    SizeTooSmall,
    #[error("Duration in blocks must be greater than 0")]
    DurationInBlocksZero,
    #[error("Block time must be greater than 0")]
    BlockTimeZero,
    #[error("Missing required field: {0}")]
    MissingField(&'static str),
}

// Helper methods for error creation
impl BatchError {
    /// Creates a `MissingField` error for the given field.
    pub fn missing_field(field: &'static str) -> Self {
        BatchError::MissingField(field)
    }
}

/// Marker traits for builder states
pub trait BuilderState {}

/// Initial state of the batch builder before any fields are set.
pub struct Initial;
impl BuilderState for Initial {}

/// State of the batch builder after a signer has been set.
pub struct WithOwner;
impl BuilderState for WithOwner {}

/// State of the batch builder after an ID has been set.
pub struct WithId;
impl BuilderState for WithId {}

/// State of the batch builder after size-related parameters have been set.
pub struct WithSize;
impl BuilderState for WithSize {}

const MIN_BUCKET_DEPTH: u8 = 16;
const IMMUTABLE_DEFAULT: bool = false;

/// Configuration container for building a `Batch`.
#[derive(Default)]
struct BatchConfig {
    id: Option<BatchId>,
    value: Option<U256>,
    owner: Option<Address>,
    depth: Option<u8>,
    bucket_depth: Option<u8>,
    immutable: Option<bool>,
}

impl BatchConfig {
    /// Builds the `Batch` from the configured parameters.
    #[track_caller]
    fn build(self) -> Batch {
        Batch {
            id: self.id.expect("Missing batch ID"),
            value: self.value,
            owner: self.owner.expect("Missing batch owner"),
            depth: self.depth.expect("Missing batch depth"),
            bucket_depth: self.bucket_depth.expect("Missing batch bucket depth"),
            immutable: self.immutable.unwrap_or(IMMUTABLE_DEFAULT),
        }
    }
}

/// A stateful builder for creating batches.
pub struct BatchBuilder<S: BuilderState> {
    config: BatchConfig,
    _state: PhantomData<S>,
}

impl<S: BuilderState> BatchBuilder<S> {
    /// Sets the value and returns a new builder instance.
    pub fn with_value(mut self, value: U256) -> Self {
        self.config.value = Some(value);
        self
    }

    /// Sets whether the batch is immutable and returns a new builder instance.
    pub fn with_immutable(mut self, immutable: bool) -> Self {
        self.config.immutable = Some(immutable);
        self
    }
}

impl BatchBuilder<Initial> {
    /// Creates a new batch builder in the initial state.
    pub fn new() -> Self {
        Self {
            config: BatchConfig::default(),
            _state: PhantomData,
        }
    }

    /// Sets the owner and transitions to `WithOwner` state.
    pub fn with_owner(mut self, owner: Address) -> BatchBuilder<WithOwner> {
        self.config.owner = Some(owner);
        BatchBuilder {
            config: self.config,
            _state: PhantomData,
        }
    }

    /// Sets the signer and transitions to `WithSigner` state.
    pub fn with_signer(self, signer: impl Signer) -> BatchBuilder<WithOwner> {
        self.with_owner(signer.address())
    }
}

impl BatchBuilder<WithOwner> {
    /// Sets the batch ID and returns a new builder instance.
    pub fn with_id(mut self, id: BatchId) -> BatchBuilder<WithId> {
        self.config.id = Some(id);
        BatchBuilder {
            config: self.config,
            _state: PhantomData,
        }
    }

    /// Derives the ID of the batch given the relationship that `id = H(msg.sender|nonce)`.
    pub fn with_id_derived_from_nonce(self, nonce: B256) -> BatchBuilder<WithId> {
        let encoded = DynSolValue::Tuple(vec![
            DynSolValue::Address(self.config.owner.unwrap()),
            DynSolValue::FixedBytes(nonce, 32),
        ])
        .abi_encode();

        let mut hasher = Keccak256::new();
        hasher.update(encoded);

        self.with_id(hasher.finalize())
    }

    /// Derives the ID of the batch given a string (`nonce` = `H(string)`).
    pub fn with_id_derived_from_string(self, string: &str) -> BatchBuilder<WithId> {
        let mut hasher = Keccak256::new();
        hasher.update(string);

        self.with_id_derived_from_nonce(hasher.finalize())
    }
}

impl BatchBuilder<WithId> {
    /// Automatically calculates the depth based on size and transitions to `WithSize` state.
    pub fn auto_size(self, size_bytes: u64) -> Result<BatchBuilder<WithSize>> {
        if size_bytes == 0 {
            return Err(BatchError::SizeTooSmall);
        }

        let depth = Batch::depth_for_size(size_bytes).max(MIN_BUCKET_DEPTH);
        self.with_depths(depth, MIN_BUCKET_DEPTH)
    }

    /// Sets the depth and bucket depth and transitions to `WithSize` state.
    /// Fails if `depth` < `bucket_depth`.
    /// Fails if `bucket_depth` < `MIN_BUCKET_DEPTH`.
    pub fn with_depths(mut self, depth: u8, bucket_depth: u8) -> Result<BatchBuilder<WithSize>> {
        if bucket_depth > depth {
            return Err(BatchError::InvalidBucketDepth(bucket_depth));
        }

        if bucket_depth < MIN_BUCKET_DEPTH {
            return Err(BatchError::InvalidBucketDepth(bucket_depth));
        }

        self.config.depth = Some(depth);
        self.config.bucket_depth = Some(bucket_depth);

        Ok(BatchBuilder {
            config: self.config,
            _state: PhantomData,
        })
    }
}

impl BatchBuilder<WithSize> {
    /// Builds the `Batch` from the configured parameters.
    pub fn build(self) -> Batch {
        self.config.build()
    }
}

/// Represents a time duration in blocks, either directly as blocks or
/// a combination of seconds and block time.
#[derive(Debug, Serialize, Deserialize)]
pub enum Duration {
    /// Time duration specified in blocks.
    Blocks(u64),
    /// Time duration specified in seconds and block time.
    Time { secs: u64, block_time: u64 },
}

impl Duration {
    /// Converts the duration to blocks.
    fn to_blocks(&self) -> Result<u64> {
        match self {
            Duration::Blocks(blocks) => Ok(*blocks),
            Duration::Time { secs, block_time } => {
                if *block_time == 0 {
                    return Err(BatchError::BlockTimeZero);
                }
                Ok(*secs / *block_time)
            }
        }
    }
}

impl From<u64> for Duration {
    /// Converts a number of blocks into a `Duration`.
    fn from(blocks: u64) -> Self {
        Duration::Blocks(blocks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloy::signers::local::PrivateKeySigner;

    fn get_test_signer() -> PrivateKeySigner {
        PrivateKeySigner::random()
    }

    #[test]
    fn test_batch_builder() {
        // Manual configuration
        let signer = get_test_signer();
        println!("Test signer: {}", signer.address());

        let batch = BatchBuilder::new()
            .with_signer(signer)
            .with_id_derived_from_string("test")
            .with_value(U256::from(100))
            .with_depths(20, 16)
            .expect("Invalid depth")
            .build();

        assert_eq!(batch.value, Some(U256::from(100)));

        let batch_extension_cost = batch.cost(
            Duration::Time {
                secs: 86400 * 30,
                block_time: 5,
            },
            U256::from(24000),
        );

        println!("Batch: {:?}", batch);
        println!("Batch extension cost: {:?}", batch_extension_cost);

        // Automatic size calculation and cost estimation
        let batch2 = BatchBuilder::new()
            .with_immutable(true)
            .with_owner(Address::ZERO)
            .with_id(BatchId::ZERO)
            .auto_size(1 << 20)
            .expect("Invalid size") // 1 MiB
            .build();
        println!("Batch 2: {:?}", batch2);

        //

        assert!(batch2.immutable());
    }
}
