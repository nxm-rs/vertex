use alloy::{
    primitives::{address, b256, Address, Keccak256, PrimitiveSignature, B256},
    signers::{local::PrivateKeySigner, Signer},
};
use bytes::{Bytes, BytesMut};
use std::sync::OnceLock;
use swarm_primitives_traits::{
    chunk::{ChunkError, Result},
    Chunk, ChunkAddress, ChunkBody, ChunkData, Signable,
};

use super::bmt_body::BMTBody;

const ID_SIZE: usize = std::mem::size_of::<B256>();
const SIGNATURE_SIZE: usize = 65;
const MIN_SOC_FIELDS_SIZE: usize = ID_SIZE + SIGNATURE_SIZE;

/// The address of the owner of the SOC for dispersed replicas.
/// Generated from the private key `0x0100000000000000000000000000000000000000000000000000000000000000`.
pub const DISPERSED_REPLICA_OWNER: Address = address!("0xdc5b20847f43d67928f49cd4f85d696b5a7617b5");
pub const DISPERSED_REPLICA_OWNER_PK: B256 =
    b256!("0x0100000000000000000000000000000000000000000000000000000000000000");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SingleOwnerChunk {
    id: B256,
    signature: PrimitiveSignature,
    body: BMTBody,
    cached_owner: OnceLock<Address>,
}

impl SingleOwnerChunk {
    /// Creates a new builder for SingleOwnerChunk
    pub fn builder() -> SingleOwnerChunkBuilder {
        SingleOwnerChunkBuilder::default()
    }

    /// Create a new SingleOwnerChunk from a given id, data and signer.
    pub async fn new(
        id: B256,
        data: impl Into<Bytes>,
        signer: impl Signer + Send + Sync,
    ) -> Result<Self> {
        let body = BMTBody::builder().data(data).build()?;
        let hash = Self::to_sign(id, &body);
        let signature = signer.sign_message(hash.as_ref()).await?;

        Ok(Self {
            id,
            signature,
            body,
            cached_owner: OnceLock::new(),
        })
    }

    /// Create a new SingleOwnerChunk from a given id, data and signature without verification.
    pub fn new_signed_unchecked(
        id: B256,
        signature: PrimitiveSignature,
        data: impl Into<Bytes>,
    ) -> Result<Self> {
        let body = BMTBody::builder().data(data).build()?;

        Ok(Self {
            id,
            signature,
            body,
            cached_owner: OnceLock::new(),
        })
    }

    pub async fn new_dispersed_replica(first_byte: u8, data: impl Into<Bytes>) -> Result<Self> {
        let body = BMTBody::builder().data(data).build()?;

        let mut id = B256::default();
        id[0] = first_byte;
        id[1..].copy_from_slice(&body.hash().as_slice()[1..]);

        let hash = Self::to_sign(id, &body);
        let signer = PrivateKeySigner::from_slice(&DISPERSED_REPLICA_OWNER_PK.as_slice()).unwrap();
        let signature = signer.sign_message(hash.as_ref()).await?;

        Ok(Self {
            id,
            signature,
            body,
            cached_owner: OnceLock::new(),
        })
    }

    /// Returns the ID of the chunk
    pub fn id(&self) -> B256 {
        self.id
    }

    fn to_sign(id: B256, body: &impl ChunkBody) -> B256 {
        let mut hasher = Keccak256::new();
        hasher.update(id);
        hasher.update(body.hash());
        hasher.finalize()
    }

    fn is_valid_replica(&self) -> bool {
        self.id[1..] == self.body.hash().as_slice()[1..]
    }
}

impl ChunkData for SingleOwnerChunk {
    fn data(&self) -> &[u8] {
        self.body.data()
    }

    fn size(&self) -> usize {
        MIN_SOC_FIELDS_SIZE + self.body.size()
    }
}

impl Chunk for SingleOwnerChunk {
    fn address(&self) -> ChunkAddress {
        let mut hasher = Keccak256::new();
        hasher.update(self.id);
        hasher.update(self.owner().as_slice());
        hasher.finalize()
    }

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
            let hash = Self::to_sign(self.id, &self.body);
            self.signature
                .recover_address_from_msg(&hash)
                .unwrap_or(Address::ZERO)
        })
    }

    fn signature(&self) -> &PrimitiveSignature {
        &self.signature
    }

    fn verify_signature(&self) -> Result<()> {
        // Dispersed replica check
        if self.owner() == DISPERSED_REPLICA_OWNER && !self.is_valid_replica() {
            return Err(ChunkError::Format("invalid dispersed replica"));
        }

        Ok(())
    }
}

impl From<SingleOwnerChunk> for Bytes {
    fn from(chunk: SingleOwnerChunk) -> Self {
        let mut bytes = BytesMut::with_capacity(chunk.size());
        bytes.extend_from_slice(chunk.id.as_ref());
        bytes.extend_from_slice(&chunk.signature().as_bytes());
        bytes.extend_from_slice(&Bytes::from(chunk.body));
        bytes.freeze()
    }
}

#[derive(Default)]
pub struct SingleOwnerChunkBuilder {
    id: Option<B256>,
    signature: Option<PrimitiveSignature>,
    data: Option<Bytes>,
}

impl SingleOwnerChunkBuilder {
    pub fn id(mut self, id: B256) -> Self {
        self.id = Some(id);
        self
    }

    pub fn signature(mut self, signature: PrimitiveSignature) -> Self {
        self.signature = Some(signature);
        self
    }

    pub fn data(mut self, data: impl Into<Bytes>) -> Self {
        self.data = Some(data.into());
        self
    }

    pub fn build(self) -> Result<SingleOwnerChunk> {
        let id = self.id.ok_or(ChunkError::missing_field("id"))?;
        let signature = self
            .signature
            .ok_or(ChunkError::missing_field("signature"))?;
        let body = BMTBody::builder()
            .data(self.data.ok_or(ChunkError::missing_field("data"))?)
            .build()?;

        Ok(SingleOwnerChunk {
            id,
            signature,
            body,
            cached_owner: OnceLock::new(),
        })
    }
}

impl TryFrom<&[u8]> for SingleOwnerChunk {
    type Error = ChunkError;

    fn try_from(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < MIN_SOC_FIELDS_SIZE {
            return Err(ChunkError::size(
                "insufficient data",
                MIN_SOC_FIELDS_SIZE,
                bytes.len(),
            ));
        }

        let id = B256::from_slice(&bytes[..ID_SIZE]);
        let signature = PrimitiveSignature::try_from(&bytes[ID_SIZE..MIN_SOC_FIELDS_SIZE])
            .map_err(ChunkError::Signature)?;

        // Use get() to safely handle the case where bytes.len() == MIN_SOC_FIELDS_SIZE
        let body_bytes = bytes.get(MIN_SOC_FIELDS_SIZE..).unwrap_or(&[]);
        let body = BMTBody::try_from(body_bytes)?;

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
    use alloy::hex;
    use alloy::primitives::{address, b256};
    use alloy::signers::local::PrivateKeySigner;

    fn get_test_wallet() -> PrivateKeySigner {
        // Test private key corresponding to address 0x8d3766440f0d7b949a5e32995d09619a7f86e632
        let pk = hex!("2c7536e3605d9c16a7a3d7b1898e529396a65c23a3bcbd4012a11cf2731b0fbc");
        PrivateKeySigner::from_slice(&pk).unwrap()
    }

    #[tokio::test]
    async fn test_new() {
        let id = B256::ZERO;
        let data = b"foo".to_vec();
        let wallet = get_test_wallet();

        let chunk = SingleOwnerChunk::new(id, data.clone(), wallet)
            .await
            .unwrap();

        assert_eq!(chunk.id(), id);
        assert_eq!(chunk.data(), &data);
    }

    #[tokio::test]
    async fn test_new_signed() {
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

    #[tokio::test]
    async fn test_chunk_conversion() {
        let id = B256::ZERO;
        let data = b"foo".to_vec();
        let wallet = get_test_wallet();

        // Create signed chunk
        let chunk = SingleOwnerChunk::new(id, data.clone(), wallet)
            .await
            .unwrap();

        // Convert to bytes
        let bytes: Bytes = chunk.clone().into();

        // Convert back from bytes
        let recovered_chunk = SingleOwnerChunk::try_from(bytes.as_ref()).unwrap();

        // Verify fields match
        assert_eq!(recovered_chunk.id(), chunk.id());
        assert_eq!(recovered_chunk.signature(), chunk.signature());
        assert_eq!(recovered_chunk.data(), chunk.data());
        assert_eq!(recovered_chunk.owner(), chunk.owner());
    }

    #[tokio::test]
    async fn test_invalid_data() {
        // Test insufficient data size
        let too_small = vec![0u8; MIN_SOC_FIELDS_SIZE - 1];
        assert!(matches!(
            SingleOwnerChunk::try_from(too_small.as_slice()),
            Err(ChunkError::Size { .. })
        ));

        // Test missing fields in builder
        let result = SingleOwnerChunk::builder().build();
        assert!(matches!(result, Err(ChunkError::MissingField("id"))));

        let result = SingleOwnerChunk::builder().id(B256::ZERO).build();
        assert!(matches!(result, Err(ChunkError::MissingField("signature"))));
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

    #[tokio::test]
    async fn test_chunk_address() {
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

    #[tokio::test]
    async fn test_invalid_signature_returns_zero_address() {
        let id = B256::ZERO;
        let data = b"test".to_vec();
        // Create an invalid signature (all zeros)
        let invalid_signature = PrimitiveSignature::try_from([0u8; 65].as_slice()).unwrap();

        let chunk = SingleOwnerChunk::new_signed_unchecked(id, invalid_signature, data).unwrap();

        assert_eq!(chunk.owner(), Address::ZERO);
    }

    #[tokio::test]
    async fn test_invalid_chunk() {
        // Base valid data
        let valid_data = get_test_chunk_data();

        // Test: Invalid signature
        let mut invalid_sig = valid_data.clone();
        invalid_sig[ID_SIZE] = 0x01; // Modify first byte of signature

        let result = SingleOwnerChunk::try_from(invalid_sig.as_slice())
            .unwrap()
            .verify(b256!(
                "9d453ebb73b2fedaaf44ceddcf7a0aa37f3e3d6453fea5841c31f0ea6d61dc85"
            ));
        assert!(result.is_err());

        // Test: Invalid data size (too small)
        let too_small = vec![0u8; MIN_SOC_FIELDS_SIZE - 1];
        assert!(matches!(
            SingleOwnerChunk::try_from(too_small.as_slice()),
            Err(ChunkError::Size { .. })
        ));
    }

    #[tokio::test]
    async fn test_dispersed_replica() {
        let test_data = b"test data".to_vec();

        // Test with different first bytes
        for first_byte in [0u8, 1, 42, 255] {
            let chunk = SingleOwnerChunk::new_dispersed_replica(first_byte, test_data.clone())
                .await
                .unwrap();

            // Verify it's recognised as a dispersed replica
            assert!(chunk.is_valid_replica());

            // Verify the first byte matches what we set
            assert_eq!(chunk.id()[0], first_byte);

            // Verify the rest of the ID matches the body hash
            let body_hash = chunk.body.hash();
            assert_eq!(&chunk.id()[1..], &body_hash.as_slice()[1..]);

            // Verify owner is correct
            assert_eq!(chunk.owner(), DISPERSED_REPLICA_OWNER);

            // Verify the chunk's address verification works
            assert!(chunk.verify(chunk.address()).is_ok());
        }
    }

    #[tokio::test]
    async fn test_invalid_dispersed_replica() {
        let test_data = b"test data".to_vec();

        let chunk = SingleOwnerChunk::new_dispersed_replica(1u8, test_data)
            .await
            .unwrap();
        let replica_address = chunk.address();
        // Serialise the chunk
        let bytes: Bytes = chunk.into();

        // Modify the last byte of the ID
        // This should make the chunk not recognised as a dispersed replica
        let mut modified_bytes = bytes.to_vec();
        modified_bytes[ID_SIZE - 1] = 0x01;

        let modified_chunk = SingleOwnerChunk::try_from(modified_bytes.as_ref()).unwrap();
        assert!(!modified_chunk.is_valid_replica());
        assert!(modified_chunk.verify(replica_address).is_err());
    }
}
