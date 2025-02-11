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
    primitives::{Address, BlockNumber, Keccak256, B256, U256},
    signers::Signer,
};
use alloy_chains::Chain;
use arbitrary::Arbitrary;
use nectar_primitives_traits::CHUNK_SIZE;
use serde::{Deserialize, Serialize};
use std::{marker::PhantomData, sync::Arc, time::Duration};
use thiserror::Error;

use super::{BatchId, ChainState};

// Type alias for Result
pub type Result<T> = std::result::Result<T, BatchError>;

// The minimum bucket depth here is specified in the `PostageStamp` contract.
// TODO: Dynamically fetch this value from the contract.
const MIN_BUCKET_DEPTH: u8 = 16;
const IMMUTABLE_DEFAULT: bool = false;

// TODO: Implement `encode` / `decode` for `Batch` for serde to/from KV storage.
/// Represents a storage unit with specific characteristics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Batch {
    /// The unique identifier of the batch, generally `H(nonce|owner)`.
    id: BatchId,
    /// The normalised balance associated with the batch, which represents a per-chunk value.
    /// If value is `None`, the batch is considered to not have been created on-chain.
    value: Option<U256>,
    /// The address that owns the batch and can sign stamps for it.
    owner: Address,
    /// Determines the number of chunks that can be signed by the batch. (2^depth)
    depth: u8,
    /// Specifies the uniformity of the batch, which is the number of bits that are used to determine
    /// the bucket in which a stamp is placed.
    bucket_depth: u8,
    /// Indicates if the batch is immutable.
    immutable: bool,
    /// The chain on which the batch is to be created / managed.
    #[serde(skip)]
    chain: Option<Arc<Chain>>,
}

impl Batch {
    /// Returns the unique identifier of the batch.
    pub fn id(&self) -> &BatchId {
        &self.id
    }

    /// Returns a `BatchBuilder` instance in the initial state.
    ///
    /// The builder pattern is used to construct batches in a step-by-step manner, ensuring that all necessary fields
    /// are properly configured. This method initialises the builder with an empty configuration and transitions it to
    /// the `Initial` state.
    pub fn builder() -> BatchBuilder<Initial> {
        BatchBuilder::default()
    }

    /// Calculates the remaining time-to-live (TTL) in blocks.
    ///
    /// If `value` <= `current_out_payment`, the batch is considered expired and the TTL is 0.
    pub fn ttl_blocks(&self, chain_state: &ChainState) -> Result<u64> {
        let value = self.value.ok_or(BatchError::BatchNotCreated)?;

        if chain_state.price.is_zero() {
            return Err(BatchError::DivisionByZero);
        }

        if value <= chain_state.payment {
            return Ok(0);
        }

        // Safety: `value` is always greater than `payment` here.
        let remaining_value = value - chain_state.payment;

        // Calculate TTL: remaining_value / price
        remaining_value
            .checked_div(chain_state.price)
            .ok_or(BatchError::ArithmeticOverflow)?
            .try_into()
            .map_err(BatchError::UintConversion)
    }

    /// Converts the remaining TTL from blocks to seconds using the block time.
    #[must_use]
    pub fn ttl_seconds(&self, blocks_remaining: u64) -> u64 {
        blocks_remaining
            * self
                .chain
                .as_ref()
                .unwrap_or(&Arc::new(Chain::dev()))
                .average_blocktime_hint()
                .unwrap_or(std::time::Duration::from_secs(12))
                .as_secs()
    }

    /// Calculates the TTL in seconds based on current out payment, price, and block time.
    pub fn ttl(&self, chain_state: &ChainState) -> Result<u64> {
        Ok(self.ttl_seconds(self.ttl_blocks(chain_state)?))
    }

    /// Determines the block number when the batch expires.
    pub fn expiry_block_number(&self, chain_state: &ChainState) -> Result<BlockNumber> {
        Ok(chain_state
            .block_number
            .saturating_add(self.ttl_blocks(chain_state)?))
    }

    /// Determines the expiration time of the batch in Unix timestamp.
    pub fn expiry(&self, chain_state: &ChainState) -> Result<u64> {
        Ok(chain_state.block_timestamp + self.ttl_seconds(self.ttl_blocks(chain_state)?))
    }

    /// Checks if the batch has expired based on the current block number.
    pub fn expired(&self, chain_state: &ChainState) -> Result<bool> {
        Ok(self.expiry_block_number(chain_state)? <= chain_state.block_number)
    }

    /// Calculates the cost of a batch for the additional target duration and current storage price.
    #[must_use]
    pub fn cost(&self, duration: Duration, price: U256) -> Result<U256> {
        let chunks = Self::chunks(self.depth);
        let chain = self.chain.as_ref().ok_or(BatchError::ChainNotSet)?;

        let blocks = duration
            .div_duration_f64(chain.average_blocktime_hint().unwrap())
            .ceil() as u64;

        Ok(U256::from(chunks) * price * U256::from(blocks))
    }

    /// Calculates the required depth for a given size in bytes.
    ///
    /// Note: Uploading data of 0 bytes is allowed and is represented by an empty chunk.
    #[must_use]
    pub fn depth_for_size(size: u64) -> u8 {
        // A minimum of 1 chunk is always required, irrespective of the size.
        let chunks = (size / CHUNK_SIZE as u64).max(1);
        ((chunks as f64).log2().ceil() as u8).max(MIN_BUCKET_DEPTH)
    }

    // Returns the number of chunks in the batch (2^depth).
    //
    // # Panics
    // If `depth` is greater than 63, this function will panic.
    #[track_caller]
    const fn chunks(depth: u8) -> u64 {
        2_u64.pow(depth as u32)
    }

    /// Returns the number of bytes that may be stored in the batch.
    pub(crate) const fn size(depth: u8) -> u64 {
        Self::chunks(depth) * CHUNK_SIZE as u64
    }

    /// Returns how many collision buckets data stamped with this batch will be split into.
    /// This is the number of bits that are used to determine the bucket in which a stamp is placed.
    pub(crate) const fn bucket_count(&self) -> u64 {
        2_u64.pow(self.bucket_depth as u32)
    }

    /// Returns the number of slots in a collision bucket.
    ///
    /// # Panics
    /// This function panics if `depth` is less than `bucket_depth`.
    pub(crate) const fn bucket_slots(&self) -> u64 {
        2_u64.pow((self.depth - self.bucket_depth) as u32)
    }
}

/// Captures errors during batch operations.
#[derive(Debug, Error)]
pub enum BatchError {
    #[error("Invalid depth: {0}")]
    InvalidDepth(u8),
    #[error("Bucket depth {0} below minimum depth {1}")]
    BucketDepthTooSmall(u8, u8),
    #[error("Chain not set for batch")]
    ChainNotSet,
    #[error("Depth must be greater than or equal to bucket depth")]
    DepthLessThanBucketDepth(u8, u8),
    #[error("Depth {0} exceeds maximum depth {1}")]
    DepthTooLarge(u8, u8),
    #[error("Division by zero error in TTL calculation")]
    DivisionByZero,
    #[error("Batch value is insufficient (less than or equal to current payment")]
    InsufficientValue,
    #[error("Arithmetic overflow in calculation")]
    ArithmeticOverflow,
    #[error("Batch has not been created on-chain")]
    BatchNotCreated,
    #[error("Uint conversion error: {0}")]
    UintConversion(#[from] alloy::primitives::ruint::FromUintError<u64>),
}

// Helper methods for error creation
impl BatchError {
    fn depth_less_than_bucket_depth(depth: u8, bucket_depth: u8) -> Self {
        BatchError::DepthLessThanBucketDepth(depth, bucket_depth)
    }

    fn bucket_depth_too_small(bucket_depth: u8) -> Self {
        BatchError::BucketDepthTooSmall(bucket_depth, MIN_BUCKET_DEPTH)
    }

    fn depth_too_large(depth: u8, max_depth: u8) -> Self {
        BatchError::DepthTooLarge(depth, max_depth)
    }
}

/// Marker traits for builder states
pub trait BuilderState {}

/// Initial state of the batch builder before any fields are set.
#[derive(Default, Clone)]
pub struct Initial;
impl BuilderState for Initial {}

/// State of the batch builder after a signer has been set.
#[derive(Clone)]
pub struct WithOwner;
impl BuilderState for WithOwner {}

/// State of the batch builder after an ID has been set.
#[derive(Clone)]
pub struct WithId;
impl BuilderState for WithId {}

/// State of the batch builder after all required fields have been set and the batch is ready to build.
#[derive(Clone)]
pub struct ReadyToBuild;
impl BuilderState for ReadyToBuild {}

/// Configuration container for building a `Batch`.
#[derive(Default, Clone)]
struct BatchConfig {
    id: Option<BatchId>,
    value: Option<U256>,
    owner: Option<Address>,
    depth: Option<u8>,
    bucket_depth: Option<u8>,
    immutable: Option<bool>,
    chain: Option<Arc<Chain>>,
}

/// A stateful builder for creating batches.
#[derive(Default, Clone)]
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

    /// Sets the chain and returns a new builder instance.
    pub fn with_chain(mut self, chain: Arc<Chain>) -> Self {
        self.config.chain = Some(chain);
        self
    }
}

impl BatchBuilder<Initial> {
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
    /// Automatically calculates the depth based on size and transitions to `ReadyToBuild` state.
    /// TODO: This **MUST** be modified to take into account *effective** utilisation.
    pub fn auto_size(self, size_bytes: u64) -> Result<BatchBuilder<ReadyToBuild>> {
        let depth = Batch::depth_for_size(size_bytes);
        self.with_depths(depth, MIN_BUCKET_DEPTH)
    }

    /// Sets the depth and bucket depth and transitions to `ReadyToBuild` state.
    /// Applies constraints on the depth and bucket depth values:
    /// 1. Ensures that `depth` >= `bucket_depth` (undefined state as collision slots available
    ///    would be negative).
    /// 2. Ensures that `bucket_depth` >= `MIN_BUCKET_DEPTH` (protocol constraint).
    /// 3. Ensures that `depth` - `bucket_depth` <= 63 (overflow prevention).
    pub fn with_depths(
        mut self,
        depth: u8,
        bucket_depth: u8,
    ) -> Result<BatchBuilder<ReadyToBuild>> {
        if depth < bucket_depth {
            return Err(BatchError::depth_less_than_bucket_depth(
                depth,
                bucket_depth,
            ));
        }

        if bucket_depth < MIN_BUCKET_DEPTH {
            return Err(BatchError::bucket_depth_too_small(bucket_depth));
        }

        if depth - bucket_depth > 63 {
            return Err(BatchError::depth_too_large(depth, 63));
        }

        self.config.depth = Some(depth);
        self.config.bucket_depth = Some(bucket_depth);

        Ok(BatchBuilder {
            config: self.config,
            _state: PhantomData,
        })
    }
}

impl BatchBuilder<ReadyToBuild> {
    /// Builds the `Batch` from the configured parameters.
    #[track_caller]
    fn build(mut self) -> Batch {
        Batch {
            id: self.config.id.take().unwrap(),
            value: self.config.value.take(),
            owner: self.config.owner.take().unwrap(),
            depth: self.config.depth.take().unwrap(),
            bucket_depth: self.config.bucket_depth.take().unwrap(),
            immutable: self.config.immutable.unwrap_or(IMMUTABLE_DEFAULT),
            chain: self.config.chain,
        }
    }
}

impl<'a> Arbitrary<'a> for Batch {
    fn arbitrary(u: &mut arbitrary::Unstructured<'a>) -> arbitrary::Result<Self> {
        // Generate valid depths ensuring bucket_depth <= depth <= 63
        let depth = u.int_in_range(MIN_BUCKET_DEPTH..=63)?;
        let bucket_depth = u.int_in_range(MIN_BUCKET_DEPTH..=depth)?;

        // Generate a random value that could be None
        let value = if bool::arbitrary(u)? {
            Some(U256::arbitrary(u)?)
        } else {
            None
        };

        // Use the builder pattern to ensure valid construction
        Ok(Batch::builder()
            .with_chain(Arc::new(Chain::from_named(
                alloy_chains::NamedChain::Gnosis,
            )))
            .with_owner(Address::arbitrary(u)?)
            .with_id(BatchId::arbitrary(u)?)
            .with_value(value.unwrap_or_default())
            .with_immutable(bool::arbitrary(u)?)
            .with_depths(depth, bucket_depth)
            .expect("depths are valid by construction")
            .build())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::signers::local::PrivateKeySigner;
    use alloy_chains::NamedChain;
    use proptest::prelude::*;
    use proptest_arbitrary_interop::arb;
    use std::time::Duration;

    // Helper function to create a batch with specific parameters
    fn create_test_batch(value: Option<U256>) -> Batch {
        let chain = Arc::new(Chain::from_named(NamedChain::Gnosis));
        let builder = match value {
            Some(value) => Batch::builder().with_value(value),
            None => Batch::builder(),
        };

        builder
            .with_owner(Address::ZERO)
            .with_id(BatchId::ZERO)
            .with_chain(chain)
            .with_depths(16, 16)
            .unwrap()
            .build()
    }

    // Helper function to create chain state
    fn create_chain_state(
        block_number: u64,
        timestamp: u64,
        price: U256,
        payment: U256,
    ) -> ChainState {
        ChainState {
            block_number,
            block_timestamp: timestamp,
            price,
            payment,
        }
    }

    // Test strategies
    fn valid_depth() -> impl Strategy<Value = u8> {
        MIN_BUCKET_DEPTH..=32u8
    }

    fn arbitrary_batch() -> impl Strategy<Value = Batch> {
        arb::<Batch>()
    }

    // Batch Tests
    proptest! {
        #[test]
        fn test_batch_properties(batch in arbitrary_batch()) {
            // Basic invariants
            prop_assert!(batch.depth >= batch.bucket_depth);
            prop_assert!(batch.bucket_depth >= MIN_BUCKET_DEPTH);
            prop_assert!(!batch.id().is_zero());
        }

        #[test]
        fn test_batch_cost_calculation(
            batch in arbitrary_batch(),
            duration_secs in 1..31_536_000u64, // 1 second to 1 year
            price in 1..1_000_000u64,
        ) {
            let duration = Duration::from_secs(duration_secs);
            let price = U256::from(price);

            if let Ok(cost) = batch.cost(duration, price) {
                // Cost should be proportional to duration and price
                prop_assert!(!cost.is_zero());

                // Test with double duration
                if let Ok(double_cost) = batch.cost(duration * 2, price) {
                    prop_assert!(double_cost > cost);
                }

                // Test with double price
                if let Ok(double_price_cost) = batch.cost(duration, price * U256::from(2)) {
                    prop_assert!(double_price_cost > cost);
                }
            }
        }

        #[test]
        fn test_depth_calculations(
            depth in valid_depth(),
            bucket_depth in 0..=32u8,
            owner in any::<Address>(),
            id in any::<BatchId>(),
        ) {
            if depth >= bucket_depth {
                let batch = Batch::builder()
                    .with_owner(owner)
                    .with_id(id)
                    .with_depths(depth, bucket_depth);

                match bucket_depth < MIN_BUCKET_DEPTH {
                    true => prop_assert!(batch.is_err()),
                    false => prop_assert!(batch.is_ok()),
                }

                if let Ok(batch) = batch {
                    let built = batch.build();
                    prop_assert_eq!(built.bucket_count(), 2u64.pow(bucket_depth as u32));
                    prop_assert_eq!(built.bucket_slots(), 2u64.pow((depth - bucket_depth) as u32));
                }
            } else {
                let batch = Batch::builder()
                    .with_owner(owner)
                    .with_id(id)
                    .with_depths(depth, bucket_depth);

                prop_assert!(batch.is_err());
            }
        }

        #[test]
        fn test_batch_builder_chain_operations(
            batch in arbitrary_batch(),
        ) {
            // Test chain operations
            let chain = Arc::new(Chain::from_named(NamedChain::Gnosis));
            let modified = Batch::builder()
                .with_chain(chain.clone())
                .with_owner(batch.owner)
                .with_id(batch.id)
                .with_depths(batch.depth, batch.bucket_depth)
                .unwrap()
                .build();

            prop_assert_eq!(modified.id(), batch.id());
            prop_assert_eq!(modified.owner, batch.owner);
        }

        #[test]
        fn test_batch_builder_auto_size(
            size in 1..10_000_000u64, // Test realistic file sizes
            owner in any::<Address>(),
            id in any::<BatchId>(),
        ) {
            let result = Batch::builder()
                .with_owner(owner)
                .with_id(id)
                .auto_size(size);

            prop_assert!(result.is_ok());

            let batch = result.unwrap().build();
            let capacity = Batch::size(batch.depth);
            prop_assert!(capacity >= size);
        }

        #[test]
        fn test_batch_builder_id_derivation(
            string in "[a-zA-Z0-9]{1,32}",
            owner in any::<Address>(),
        ) {
            let batch = Batch::builder()
                .with_owner(owner)
                .with_id_derived_from_string(&string)
                .with_depths(MIN_BUCKET_DEPTH, MIN_BUCKET_DEPTH)
                .unwrap()
                .build();

            // Verify the derived ID is deterministic
            let batch2 = Batch::builder()
                .with_owner(owner)
                .with_id_derived_from_string(&string)
                .with_depths(MIN_BUCKET_DEPTH, MIN_BUCKET_DEPTH)
                .unwrap()
                .build();

            prop_assert_eq!(batch.id(), batch2.id());
        }

        #[test]
        fn test_builder_state_transitions(
            id in any::<BatchId>(),
            depth in valid_depth(),
            bucket_depth in MIN_BUCKET_DEPTH..=32u8,
            value in any::<U256>(),
            immutable in any::<bool>(),
        ) {
            let signer = PrivateKeySigner::random();
            let builder = Batch::builder()
                .with_value(value)
                .with_immutable(immutable)
                .with_signer(signer);

            // Test ID derivation path
            let with_derived_id = builder.clone()
                .with_id_derived_from_string("test");

            // Test explicit ID path
            let with_explicit_id = builder
                .with_id(id);

            // Both paths should succeed with valid depths
            if depth >= bucket_depth {
                prop_assert!(with_derived_id.with_depths(depth, bucket_depth).is_ok());
                prop_assert!(with_explicit_id.with_depths(depth, bucket_depth).is_ok());
            } else {
                prop_assert!(with_derived_id.with_depths(depth, bucket_depth).is_err());
                prop_assert!(with_explicit_id.with_depths(depth, bucket_depth).is_err());
            }
        }
    }

    // Additional utility tests
    #[test]
    fn test_batch_builder_error_conditions() {
        // Test invalid depth combinations
        assert!(Batch::builder()
            .with_owner(Address::random())
            .with_id(BatchId::random())
            .with_depths(MIN_BUCKET_DEPTH - 1, MIN_BUCKET_DEPTH)
            .is_err());

        // Test bucket depth > depth
        assert!(Batch::builder()
            .with_owner(Address::random())
            .with_id(BatchId::random())
            .with_depths(16, 17)
            .is_err());

        // Test maximum depth overflow
        assert!(Batch::builder()
            .with_owner(Address::random())
            .with_id(BatchId::random())
            .with_depths(255, MIN_BUCKET_DEPTH)
            .is_err());
    }

    #[test]
    fn test_ttl_blocks_edge_cases() {
        let owner = Address::random();
        let id = BatchId::random();

        // Create a basic batch
        let batch = Batch::builder()
            .with_owner(owner)
            .with_id(id)
            .with_depths(16, 16)
            .unwrap()
            .with_value(U256::from(1000))
            .build();

        // Test division by zero
        let zero_price_state = ChainState {
            block_number: 0,
            block_timestamp: 0,
            price: U256::ZERO,
            payment: U256::ZERO,
        };
        assert!(matches!(
            batch.ttl_blocks(&zero_price_state),
            Err(BatchError::DivisionByZero)
        ));

        // Test insufficient value
        let high_payment_state = ChainState {
            block_number: 0,
            block_timestamp: 0,
            price: U256::from(1),
            payment: U256::from(2000), // Higher than batch value
        };
        assert!(matches!(batch.ttl_blocks(&high_payment_state), Ok(0)));

        // Test batch not created
        let uncreated_batch = Batch::builder()
            .with_owner(owner)
            .with_id(id)
            .with_depths(16, 16)
            .unwrap()
            .build();

        let normal_state = ChainState {
            block_number: 0,
            block_timestamp: 0,
            price: U256::from(1),
            payment: U256::ZERO,
        };
        assert!(matches!(
            uncreated_batch.ttl_blocks(&normal_state),
            Err(BatchError::BatchNotCreated)
        ));
    }

    #[test]
    fn test_ttl_basic_calculation() {
        let batch = create_test_batch(Some(U256::from(1000)));
        let chain_state = create_chain_state(
            100,
            1000,
            U256::from(10), // price
            U256::from(0),  // payment
        );

        // Expected: (1000 - 0) / 10 = 100 blocks
        let ttl_blocks = batch.ttl_blocks(&chain_state).unwrap();
        assert_eq!(ttl_blocks, 100, "TTL blocks calculation incorrect");

        // With 5-second blocks (default)
        let ttl_seconds = batch.ttl_seconds(ttl_blocks);
        assert_eq!(ttl_seconds, 500, "TTL seconds calculation incorrect");
    }

    #[test]
    fn test_ttl_with_existing_payment() {
        let batch = create_test_batch(Some(U256::from(1000)));
        let chain_state = create_chain_state(
            100,
            1000,
            U256::from(10),  // price
            U256::from(500), // payment
        );

        // Expected: (1000 - 500) / 10 = 50 blocks
        let ttl_blocks = batch.ttl_blocks(&chain_state).unwrap();
        assert_eq!(
            ttl_blocks, 50,
            "TTL blocks calculation with payment incorrect"
        );
    }

    #[test]
    fn test_ttl_expired_batch() {
        let batch = create_test_batch(Some(U256::from(1000)));
        let chain_state = create_chain_state(
            100,
            1000,
            U256::from(10),   // price
            U256::from(1000), // payment equals value
        );

        let ttl_blocks = batch.ttl_blocks(&chain_state).unwrap();
        assert_eq!(ttl_blocks, 0, "Expired batch should have 0 TTL");
    }

    #[test]
    fn test_ttl_uncreated_batch() {
        let batch = create_test_batch(None);
        let chain_state = create_chain_state(100, 1000, U256::from(10), U256::from(0));

        assert!(matches!(
            batch.ttl_blocks(&chain_state),
            Err(BatchError::BatchNotCreated)
        ));
    }

    #[test]
    fn test_ttl_zero_price() {
        let batch = create_test_batch(Some(U256::from(1000)));
        let chain_state = create_chain_state(100, 1000, U256::ZERO, U256::from(0));

        assert!(matches!(
            batch.ttl_blocks(&chain_state),
            Err(BatchError::DivisionByZero)
        ));
    }

    #[test]
    fn test_expiry_calculation() {
        let batch = create_test_batch(Some(U256::from(1000)));
        let chain_state = create_chain_state(
            100,  // current block
            1000, // current timestamp
            U256::from(10),
            U256::from(0),
        );

        // TTL should be 100 blocks
        let expiry_block = batch.expiry_block_number(&chain_state).unwrap();
        assert_eq!(expiry_block, 200, "Expiry block number incorrect");

        // With 5-second blocks
        let expiry_time = batch.expiry(&chain_state).unwrap();
        assert_eq!(expiry_time, 1500, "Expiry timestamp incorrect");
    }

    #[test]
    fn test_expired_status() {
        let batch = create_test_batch(Some(U256::from(1000)));

        // Test not expired
        let chain_state = create_chain_state(100, 1000, U256::from(10), U256::from(0));
        assert!(
            !batch.expired(&chain_state).unwrap(),
            "Batch should not be expired"
        );

        // Test expired
        let expired_state = create_chain_state(
            100,
            1000,
            U256::from(10),
            U256::from(1000), // payment equals value
        );
        assert!(
            batch.expired(&expired_state).unwrap(),
            "Batch should be expired"
        );
    }

    #[test]
    fn test_ttl_with_other_chain() {
        // Create a chain with 5-second block time
        let custom_chain = Chain::from_named(NamedChain::Sepolia);

        let batch = Batch::builder()
            .with_owner(Address::ZERO)
            .with_id(BatchId::ZERO)
            .with_chain(Arc::new(custom_chain))
            .with_value(U256::from(1000))
            .with_depths(16, 16)
            .unwrap()
            .build();

        let chain_state = create_chain_state(100, 1000, U256::from(10), U256::from(0));

        let ttl_blocks = batch.ttl_blocks(&chain_state).unwrap();
        assert_eq!(ttl_blocks, 100, "TTL blocks calculation incorrect");

        // With 12-second blocks (sepolia)
        let ttl_seconds = batch.ttl(&chain_state).unwrap();
        assert_eq!(
            ttl_seconds, 1200,
            "TTL seconds calculation incorrect with custom chain"
        );
    }

    #[test]
    fn test_large_values() {
        let large_value = U256::from(u64::MAX);
        let batch = create_test_batch(Some(large_value));
        let chain_state = create_chain_state(
            100,
            1000,
            U256::from(1_000_000_000), // Large price to prevent overflow
            U256::from(0),
        );

        let ttl_blocks = batch.ttl_blocks(&chain_state).unwrap();
        assert!(
            ttl_blocks > 0,
            "TTL blocks should be calculated for large values"
        );
    }
}
