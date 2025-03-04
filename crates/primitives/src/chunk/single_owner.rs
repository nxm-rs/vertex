//! The `chunk::SingleOwner` module provides functionality for managing chunks with ownership and digital signatures.
//!
//! This module includes:
//!
//! - The `SingleOwnerChunk` struct: Represents a chunk of data with a unique identifier and digital signature, and an
//!   underlying BMT body containing data and metadata.
//! - A builder pattern (`SingleOwnerChunkBuilder`): Facilitates the creation of `SingleOwnerChunk` for ensuring that
//!   all necessary fields are properly configured in a structured manner.
use alloy_primitives::{address, b256, Address, Keccak256, PrimitiveSignature, B256};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use bytes::{Bytes, BytesMut};
use nectar_primitives_traits::{
    chunk::{ChunkError, Result},
    Chunk, ChunkAddress, ChunkBody, ChunkData, Signable,
};
use std::{marker::PhantomData, sync::OnceLock};

use super::bmt_body::BMTBody;

const ID_SIZE: usize = std::mem::size_of::<B256>();
const SIGNATURE_SIZE: usize = 65;
const MIN_SOC_FIELDS_SIZE: usize = ID_SIZE + SIGNATURE_SIZE;

/// The address of the owner of the SOC for dispersed replicas.
pub const DISPERSED_REPLICA_OWNER: Address = address!("0xdc5b20847f43d67928f49cd4f85d696b5a7617b5");
/// Generated from the private key `0x0100000000000000000000000000000000000000000000000000000000000000`.
pub const DISPERSED_REPLICA_OWNER_PK: B256 =
    b256!("0x0100000000000000000000000000000000000000000000000000000000000000");

/// The `SingleOwnerChunk` struct represents a chunk of data with ownership and digital signature.
///
/// This module includes:
///
/// - The `SingleOwnerChunk` struct: Contains the ID, signature, and BMT body of the chunk.
/// - A builder pattern (`SingleOwnerChunkBuilder`): Facilitates the creation of SingleOwnerChunk for setting
///   various parameters in a structured manner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SingleOwnerChunk {
    /// An identifier for the chunk that is used to calculate the chunk's address.
    id: B256,
    /// A digital signature of the chunk's ID and body hash.
    signature: PrimitiveSignature,
    /// The underlying BMT body containing data and metadata.
    body: BMTBody,
    /// Cache the owner address for efficient retrieval.
    cached_owner: OnceLock<Address>,
}

impl SingleOwnerChunk {
    /// Creates a new builder for SingleOwnerChunk
    pub fn builder() -> SingleOwnerChunkBuilder<Initial, SingleOwnerChunk> {
        SingleOwnerChunkBuilder::default()
    }

    /// Create a new `SingleOwnerChunk` with the given ID, data, and signer.
    ///
    /// # Arguments
    /// * `id` - The unique identifier for the chunk.
    /// * `data` - The raw data content to encapsulate in the chunk.
    /// * `signer` - The signer used to sign the chunk's ID and body hash.
    pub fn new(id: B256, data: impl Into<Bytes>, signer: impl SignerSync) -> Result<Self> {
        Ok(Self::builder()
            .with_body(BMTBody::builder().auto_from_data(data)?.build()?)?
            .with_id(id)
            .with_signer(signer)?
            .build()?)
    }

    /// Create a new `SingleOwnerChunk` with the given ID, data, and signer.
    ///
    /// # Arguments
    /// * `id` - The unique identifier for the chunk.
    /// * `data` - The raw data content to encapsulate in the chunk.
    /// * `signer` - The signer used to sign the chunk's ID and body hash.
    pub fn new_signed_unchecked(
        id: B256,
        signature: PrimitiveSignature,
        data: impl Into<Bytes>,
    ) -> Result<Self> {
        let body = BMTBody::builder().auto_from_data(data)?.build()?;

        Ok(Self {
            id,
            signature,
            body,
            cached_owner: OnceLock::new(),
        })
    }

    /// Create a new `SingleOwnerChunk` as a dispersed replica.
    ///
    /// # Arguments
    /// * `mined_byte` - The first byte of the chunk ID.
    /// * `body` - The underlying BMT body containing data and metadata.
    pub fn new_dispersed_replica(mined_byte: u8, body: BMTBody) -> Result<Self> {
        Self::builder()
            .with_body(body)?
            .dispersed_replica(mined_byte)?
            .build()
    }

    /// Returns the ID of the chunk
    pub fn id(&self) -> B256 {
        self.id
    }

    // Computes the data to sign for the chunk.
    fn to_sign(id: &B256, body: &impl ChunkBody) -> B256 {
        let mut hasher = Keccak256::new();
        hasher.update(id);
        hasher.update(body.hash());
        hasher.finalize()
    }

    // Checks if the chunk is a valid dispersed replica.
    fn is_valid_replica(&self) -> bool {
        self.id[1..] == self.body.hash().as_slice()[1..]
    }
}

impl ChunkData for SingleOwnerChunk {
    fn data(&self) -> &Bytes {
        self.body.data()
    }

    fn size(&self) -> usize {
        MIN_SOC_FIELDS_SIZE + self.body.size()
    }
}

impl Chunk for SingleOwnerChunk {
    /// Returns the address of a `SingleOwnerChunk` by hashing the ID and owner address.
    fn address(&self) -> ChunkAddress {
        let mut hasher = Keccak256::new();
        hasher.update(self.id);
        hasher.update(self.owner().as_slice());
        hasher.finalize()
    }

    /// Verifies that the chunk's address matches an expected address.
    ///
    /// # Arguments
    /// * `expected` - The expected address of the chunk.
    fn verify(&self, expected: ChunkAddress) -> Result<()> {
        let actual = self.address();

        // Verify signature recoverability and check for dispersed replica
        self.verify_signature()?;

        if actual != expected {
            return Err(ChunkError::verification(
                "address mismatch",
                expected,
                actual,
            ));
        }

        Ok(())
    }
}

impl Signable for SingleOwnerChunk {
    fn owner(&self) -> Address {
        *self.cached_owner.get_or_init(|| {
            let hash = Self::to_sign(&self.id, &self.body);
            self.signature
                .recover_address_from_msg(hash)
                .unwrap_or(Address::ZERO)
        })
    }

    fn signature(&self) -> &PrimitiveSignature {
        &self.signature
    }

    fn verify_signature(&self) -> Result<()> {
        // Dispersed replica check
        if self.owner() == DISPERSED_REPLICA_OWNER && !self.is_valid_replica() {
            return Err(ChunkError::format("invalid dispersed replica"));
        }

        Ok(())
    }
}

impl From<SingleOwnerChunk> for Bytes {
    fn from(chunk: SingleOwnerChunk) -> Self {
        let mut bytes = BytesMut::with_capacity(chunk.size());
        bytes.extend_from_slice(chunk.id.as_ref());
        bytes.extend_from_slice(&chunk.signature().as_bytes());
        bytes.extend_from_slice(Bytes::from(chunk.body).as_ref());
        bytes.freeze()
    }
}

/// Marker traits for builder states
pub trait BuilderState {}

#[derive(Default)]
pub struct Initial;
impl BuilderState for Initial {}

/// State of the `SingleOwnerChunk` builder after body has been set.
pub struct WithBody;
impl BuilderState for WithBody {}

/// State of the `SingleOwnerChunk` builder after ID has been set.
pub struct WithId;
impl BuilderState for WithId {}

/// State of the `SingleOwnerChunk` builder after all fields have been set.
pub struct ReadyToBuild;
impl BuilderState for ReadyToBuild {}

/// A stateful builder for creating `SingleOwnerChunk` instances.
///
/// This builder pattern ensures that all required fields are properly configured before building a `SingleOwnerChunk`.
/// It enforces the following sequence of states:
/// 1. Initial (no fields set)
/// 2. WithBody (body set)
/// 3. WithId (ID set)
/// 4. ReadyToBuild (all fields set, including signature)
pub struct SingleOwnerChunkBuilder<S: BuilderState, T = SingleOwnerChunk> {
    config: SingleOwnerChunkConfig,
    _state: PhantomData<S>,
    _output: PhantomData<T>,
}

#[derive(Default)]
struct SingleOwnerChunkConfig {
    id: Option<B256>,
    signature: Option<PrimitiveSignature>,
    body: Option<BMTBody>,
}

impl<T: From<SingleOwnerChunk>> Default for SingleOwnerChunkBuilder<Initial, T> {
    fn default() -> Self {
        Self {
            config: SingleOwnerChunkConfig::default(),
            _state: PhantomData,
            _output: PhantomData,
        }
    }
}

impl<T: From<SingleOwnerChunk>> SingleOwnerChunkBuilder<Initial, T> {
    /// Sets the data and transitions to WithBody state.
    pub fn with_body(mut self, body: BMTBody) -> Result<SingleOwnerChunkBuilder<WithBody, T>> {
        self.config.body = Some(body);

        Ok(SingleOwnerChunkBuilder {
            config: self.config,
            _state: PhantomData,
            _output: PhantomData,
        })
    }
}

impl<T: From<SingleOwnerChunk>> SingleOwnerChunkBuilder<WithBody, T> {
    /// Sets the ID and transitions to the WithId state.
    pub fn with_id(mut self, id: B256) -> SingleOwnerChunkBuilder<WithId, T> {
        self.config.id = Some(id);
        SingleOwnerChunkBuilder {
            config: self.config,
            _state: PhantomData,
            _output: PhantomData,
        }
    }

    /// Creates a new dispersed replica chunk with the given first byte and transitions to ReadyToBuild state.
    pub fn dispersed_replica(
        self,
        first_byte: u8,
    ) -> Result<SingleOwnerChunkBuilder<ReadyToBuild, T>> {
        let body = self.config.body.as_ref().unwrap();
        let mut id = B256::default();
        id[0] = first_byte;
        id[1..].copy_from_slice(&body.hash().as_slice()[1..]);

        let hash = SingleOwnerChunk::to_sign(&id, body);
        let signer = PrivateKeySigner::from_slice(DISPERSED_REPLICA_OWNER_PK.as_slice()).unwrap();

        self.with_id(id)
            .with_signature(signer.sign_message_sync(hash.as_ref())?)
    }
}

impl<T: From<SingleOwnerChunk>> SingleOwnerChunkBuilder<WithId, T> {
    /// Sets the signature by signing with signer and transitions to ReadyToBuild.
    pub fn with_signer(
        self,
        signer: impl SignerSync,
    ) -> Result<SingleOwnerChunkBuilder<ReadyToBuild, T>> {
        let body = self.config.body.as_ref().unwrap();
        let id = self.config.id.as_ref().unwrap();
        let hash = SingleOwnerChunk::to_sign(id, body);
        let signature = signer.sign_message_sync(hash.as_ref())?;

        self.with_signature(signature)
    }

    /// Sets the signature to the prescribed pre-signed signature, and transitions to ReadyToBuild.
    pub fn with_signature(
        mut self,
        signature: PrimitiveSignature,
    ) -> Result<SingleOwnerChunkBuilder<ReadyToBuild, T>> {
        self.config.signature = Some(signature);

        Ok(SingleOwnerChunkBuilder {
            config: self.config,
            _state: PhantomData,
            _output: PhantomData,
        })
    }
}

impl<T: From<SingleOwnerChunk>> SingleOwnerChunkBuilder<ReadyToBuild, T> {
    /// Builds the final SingleOwnerChunk
    pub fn build(mut self) -> Result<T> {
        let chunk = SingleOwnerChunk {
            id: self.config.id.take().unwrap(),
            signature: self.config.signature.take().unwrap(),
            body: self.config.body.take().unwrap(),
            cached_owner: OnceLock::new(),
        };

        Ok(T::from(chunk))
    }
}

impl TryFrom<Bytes> for SingleOwnerChunk {
    type Error = ChunkError;

    fn try_from(mut bytes: Bytes) -> Result<Self> {
        if bytes.len() < MIN_SOC_FIELDS_SIZE {
            return Err(ChunkError::size(
                "insufficient data",
                MIN_SOC_FIELDS_SIZE,
                bytes.len(),
            ));
        }

        let id = B256::from_slice(&bytes.split_to(ID_SIZE));
        let signature = PrimitiveSignature::try_from(bytes.split_to(SIGNATURE_SIZE).as_ref())
            .map_err(ChunkError::Signature)?;

        // bytes now contains only body data
        let body = BMTBody::try_from(bytes)?;

        Ok(Self {
            id,
            signature,
            body,
            cached_owner: OnceLock::new(),
        })
    }
}

impl TryFrom<&[u8]> for SingleOwnerChunk {
    type Error = ChunkError;

    fn try_from(buf: &[u8]) -> Result<Self> {
        Self::try_from(Bytes::copy_from_slice(buf))
    }
}

impl<'a> arbitrary::Arbitrary<'a> for SingleOwnerChunk {
    fn arbitrary(u: &mut arbitrary::Unstructured<'a>) -> arbitrary::Result<Self> {
        let id = B256::arbitrary(u)?;
        let body = BMTBody::arbitrary(u)?;

        let signer = PrivateKeySigner::random();
        let hash = SingleOwnerChunk::to_sign(&id, &body);
        let signature = signer.sign_message_sync(hash.as_ref()).unwrap();

        Ok(Self {
            id,
            signature,
            body,
            cached_owner: OnceLock::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::hex;
    use nectar_primitives_traits::CHUNK_SIZE;
    use proptest::prelude::*;
    use proptest_arbitrary_interop::arb;

    fn get_test_wallet() -> PrivateKeySigner {
        // Test private key corresponding to address 0x8d3766440f0d7b949a5e32995d09619a7f86e632
        let pk = hex!("2c7536e3605d9c16a7a3d7b1898e529396a65c23a3bcbd4012a11cf2731b0fbc");
        PrivateKeySigner::from_slice(&pk).unwrap()
    }

    // Strategy for generating SingleOwnerChunk using the Arbitrary implementation
    fn chunk_strategy() -> impl Strategy<Value = SingleOwnerChunk> {
        arb::<SingleOwnerChunk>()
    }

    proptest! {
        #[test]
        fn test_chunk_properties(chunk in chunk_strategy()) {
            // Test basic properties
            prop_assert!(!chunk.id().is_zero());
            prop_assert!(!chunk.data().is_empty());
            prop_assert!(chunk.size() >= MIN_SOC_FIELDS_SIZE);

            // Test round-trip conversion
            let bytes: Bytes = chunk.clone().into();
            let decoded = SingleOwnerChunk::try_from(bytes.as_ref()).unwrap();
            prop_assert_eq!(chunk.id(), decoded.id());
            prop_assert_eq!(chunk.signature(), decoded.signature());
            prop_assert_eq!(chunk.data(), decoded.data());
            prop_assert_eq!(chunk.owner(), decoded.owner());

            // Test address verification
            let address = chunk.address();
            prop_assert!(chunk.verify(address).is_ok());
        }

        #[test]
        fn test_dispersed_replica_properties(first_byte in any::<u8>(), data in proptest::collection::vec(any::<u8>(), 1..CHUNK_SIZE)) {
            let chunk = SingleOwnerChunk::new_dispersed_replica(first_byte, BMTBody::builder().auto_from_data(data).unwrap().build().unwrap()).unwrap();

            // Verify it's recognised as a dispersed replica
            prop_assert!(chunk.is_valid_replica());
            prop_assert_eq!(chunk.id()[0], first_byte);
            prop_assert_eq!(chunk.owner(), DISPERSED_REPLICA_OWNER);

            // Verify chunk address
            prop_assert!(chunk.verify(chunk.address()).is_ok());
        }

        #[test]
        fn test_chunk_creation(id in arb::<B256>(), data in proptest::collection::vec(any::<u8>(), 1..CHUNK_SIZE)) {
            let wallet = get_test_wallet();

            // Test creation through builder
            let chunk = SingleOwnerChunk::builder()
                .with_body(
                    BMTBody::builder()
                        .auto_from_data(data.clone())
                        .unwrap()
                        .build()
                        .unwrap(),
                )
                .unwrap()
                .with_id(id)
                .with_signer(&wallet)
                .unwrap()
                .build()
                .unwrap();

            prop_assert_eq!(chunk.id(), id);
            prop_assert_eq!(chunk.data(), &data);
            prop_assert!(!chunk.owner().is_zero());
        }

        #[test]
        fn test_dispersed_replica_mismatched_address(first_byte in any::<u8>(), data in proptest::collection::vec(any::<u8>(), 1..CHUNK_SIZE)) {
            let chunk = SingleOwnerChunk::builder().with_body(
                BMTBody::builder()
                    .auto_from_data(data.clone())
                    .unwrap()
                    .build()
                    .unwrap(),
            ).unwrap().dispersed_replica(first_byte).unwrap().build().unwrap();
            let replica_address = chunk.address();
            // Serialise the chunk
            let bytes: Bytes = chunk.into();

            // Modify the ID (31 bytes), except the first byte to be random.
            // This should make the chunk not recognised as a dispersed replica
            let mut modified_bytes = bytes.to_vec();
            modified_bytes[1..ID_SIZE].copy_from_slice(&[0x01; 31]);

            let modified_chunk = SingleOwnerChunk::try_from(modified_bytes.as_slice()).unwrap();
            prop_assert!(!modified_chunk.is_valid_replica());
            prop_assert!(modified_chunk.verify(replica_address).is_err());
        }

        #[test]
        fn test_chunk_invalid_signature(id in arb::<B256>(), data in proptest::collection::vec(any::<u8>(), 1..CHUNK_SIZE)) {
            let wallet = get_test_wallet();

            // Test creation through builder
            let chunk = SingleOwnerChunk::new(id, data.clone(), wallet).unwrap();
            let original_address = chunk.address();

            // Serialise the chunk
            let bytes: Bytes = chunk.into();

            // Modify the signature (65 bytes), except the first byte to be random.
            // This should make the chunk not recognised as a dispersed replica
            let mut modified_bytes = bytes.to_vec();
            modified_bytes[ID_SIZE..ID_SIZE + 65].copy_from_slice(&[0xff; 65]);

            let modified_chunk = SingleOwnerChunk::try_from(modified_bytes.as_slice()).unwrap();
            prop_assert!(modified_chunk.verify(original_address).is_err());
            prop_assert!(modified_chunk.owner() == Address::ZERO);
        }

        #[test]
        fn test_chunk_too_small(data in proptest::collection::vec(any::<u8>(), 1..MIN_SOC_FIELDS_SIZE)) {
            // Test insufficient data size
            let chunk = SingleOwnerChunk::try_from(data.as_slice());
            prop_assert!(chunk.is_err());
        }
    }

    #[test]
    fn test_new() {
        let id = B256::ZERO;
        let data = b"foo".to_vec();
        let wallet = get_test_wallet();

        let chunk = SingleOwnerChunk::new(id, data.clone(), wallet).unwrap();

        assert_eq!(chunk.id(), id);
        assert_eq!(chunk.data(), &data);
    }

    #[test]
    fn test_new_signed() {
        let id = B256::ZERO;
        let data = b"foo".to_vec();

        // Known good signature from Go tests
        let sig = hex!("5acd384febc133b7b245e5ddc62d82d2cded9182d2716126cd8844509af65a053deb418208027f548e3e88343af6f84a8772fb3cebc0a1833a0ea7ec0c1348311b");
        let signature = PrimitiveSignature::try_from(sig.as_slice()).unwrap();

        let chunk = SingleOwnerChunk::new_signed_unchecked(id, signature, data.clone()).unwrap();

        assert_eq!(chunk.id(), id);
        assert_eq!(chunk.data(), &data);
        assert_eq!(chunk.signature().as_bytes(), sig);

        // Verify owner address matches expected
        let expected_owner = address!("8d3766440f0d7b949a5e32995d09619a7f86e632");
        assert_eq!(chunk.owner(), expected_owner);
    }

    fn get_test_chunk_data() -> Vec<u8> {
        hex!(
            "000000000000000000000000000000000000000000000000000000000000000\
            05acd384febc133b7b245e5ddc62d82d2cded9182d2716126cd8844509af65a05\
            3deb418208027f548e3e88343af6f84a8772fb3cebc0a1833a0ea7ec0c134831\
            1b0300000000000000666f6f"
        )
        .to_vec()
    }

    #[test]
    fn test_chunk_address() {
        // Should parse successfully
        let chunk = SingleOwnerChunk::try_from(get_test_chunk_data().as_slice()).unwrap();

        // Verify expected owner
        let expected_owner = address!("8d3766440f0d7b949a5e32995d09619a7f86e632");
        assert_eq!(chunk.owner(), expected_owner);

        // Verify expected address
        let expected_address =
            b256!("9d453ebb73b2fedaaf44ceddcf7a0aa37f3e3d6453fea5841c31f0ea6d61dc85");
        assert_eq!(chunk.address(), expected_address);
    }

    #[test]
    fn test_invalid_dispersed_replica() -> Result<()> {
        let test_data = b"test data".to_vec();
        let dispersed_replica_wallet =
            PrivateKeySigner::from_slice(&DISPERSED_REPLICA_OWNER_PK.as_slice()).unwrap();

        let chunk = SingleOwnerChunk::builder()
            .with_body(
                BMTBody::builder()
                    .auto_from_data(test_data.clone())?
                    .build()?,
            )?
            .with_id(B256::ZERO)
            .with_signer(dispersed_replica_wallet)?
            .build()?;
        let replica_address = chunk.address();

        assert!(!chunk.is_valid_replica());
        assert!(matches!(
            chunk.verify(replica_address),
            Err(ChunkError::Format { .. })
        ));

        Ok(())
    }
}
