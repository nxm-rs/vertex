//! The `chunk::ContentChunk` module provides data structures and utilities for managing content chunks in a
//! decentralised storage network context. A content chunk wraps a [`BMTBody`].
//!
//! This module includes:
//!
//! - The `ContentChunk` struct: Represents a content chunk which contains a BMT body.
//! - A builder pattern (`ContentChunkBuilder`): Facilitates the creation of content chunks for setting
//!   various parameters in a structured manner.
use super::bmt_body::{BMTBody, BMTBodyBuilder, Initial};
use bytes::Bytes;
use nectar_primitives_traits::{
    chunk::{ChunkError, Result},
    Chunk, ChunkAddress, ChunkBody, ChunkData,
};

#[derive(Debug, Clone, PartialEq, Eq, arbitrary::Arbitrary)]
pub struct ContentChunk {
    /// The underlying BMT body which contains the data and metadata for this content chunk.
    body: BMTBody,
}

impl ContentChunk {
    /// Creates a new builder for ContentChunk
    pub fn builder() -> BMTBodyBuilder<Initial, ContentChunk> {
        BMTBodyBuilder::default()
    }

    /// Create a new `ContentChunk` with the given data. Metadata (span) is automatically calculated.
    ///
    /// # Arguments
    /// * `data` - The raw data content to encapsulate in the chunk.
    pub fn new(data: impl Into<Bytes>) -> Result<Self> {
        Ok(BMTBody::builder().auto_from_data(data)?.build()?.into())
    }

    /// Returns the span value
    pub fn span(&self) -> u64 {
        self.body.span()
    }
}

impl ChunkData for ContentChunk {
    fn data(&self) -> &Bytes {
        self.body.data()
    }

    fn size(&self) -> usize {
        self.body.size()
    }
}

impl Chunk for ContentChunk {
    /// The address of a `ContentChunk` is the hash of its body.
    fn address(&self) -> ChunkAddress {
        self.body.hash()
    }
}

impl From<ContentChunk> for Bytes {
    fn from(chunk: ContentChunk) -> Self {
        chunk.body.into()
    }
}

impl TryFrom<Bytes> for ContentChunk {
    type Error = ChunkError;

    fn try_from(buf: Bytes) -> Result<Self> {
        Ok(Self {
            body: BMTBody::try_from(buf)?,
        })
    }
}

impl TryFrom<&[u8]> for ContentChunk {
    type Error = ChunkError;

    fn try_from(buf: &[u8]) -> Result<Self> {
        Self::try_from(Bytes::copy_from_slice(buf))
    }
}

impl From<BMTBody> for ContentChunk {
    fn from(body: BMTBody) -> Self {
        Self { body }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::b256;
    use nectar_primitives_traits::CHUNK_SIZE;
    use proptest::prelude::*;
    use proptest_arbitrary_interop::arb;

    // Strategy for generating ContentChunk using the Arbitrary implementation
    fn chunk_strategy() -> impl Strategy<Value = ContentChunk> {
        arb::<ContentChunk>()
    }

    proptest! {
        #[test]
        fn test_chunk_properties(chunk in chunk_strategy()) {
            // Test basic properties
            prop_assert!(chunk.span() <= u64::MAX);
            prop_assert!(chunk.data().len() <= CHUNK_SIZE);
            prop_assert_eq!(chunk.size(), 8 + chunk.data().len());

            // Test round-trip conversion
            let bytes: Bytes = chunk.clone().into();
            let decoded = ContentChunk::try_from(bytes).unwrap();
            prop_assert_eq!(chunk.address(), decoded.address());
            prop_assert_eq!(chunk.data(), decoded.data());
            prop_assert_eq!(chunk.span(), decoded.span());
        }

        #[test]
        fn test_builder_pattern(data in proptest::collection::vec(any::<u8>(), 0..CHUNK_SIZE)) {
            let chunk = ContentChunk::builder()
                .auto_from_data(data.clone())
                .unwrap()
                .build()
                .unwrap();

            prop_assert_eq!(chunk.data(), &data);
            prop_assert_eq!(chunk.span(), data.len() as u64);
        }

        #[test]
        fn test_new_content_chunk(data in proptest::collection::vec(any::<u8>(), 0..CHUNK_SIZE)) {
            let chunk = ContentChunk::new(data.clone()).unwrap();

            prop_assert_eq!(chunk.data(), &data);
            prop_assert_eq!(chunk.span(), data.len() as u64);
            prop_assert!(!chunk.address().is_zero());
        }

        #[test]
        fn test_chunk_size_validation(data in proptest::collection::vec(any::<u8>(), CHUNK_SIZE + 1..CHUNK_SIZE * 2)) {
            let result = ContentChunk::new(data);
            prop_assert_eq!(matches!(result, Err(ChunkError::Size { .. })), true);
        }

        #[test]
        fn test_empty_and_edge_cases(size in 0usize..=10usize) {
            // Test with empty or small data
            let data = vec![0u8; size];
            let chunk = ContentChunk::new(data.clone()).unwrap();

            prop_assert_eq!(chunk.data().len(), size);
            prop_assert_eq!(chunk.span(), size as u64);
            prop_assert_eq!(chunk.size(), 8 + size);
        }

        #[test]
        fn test_deserialize_invalid_chunks(data in proptest::collection::vec(any::<u8>(), 0..8)) {
            let result = ContentChunk::try_from(data.as_slice());
            prop_assert_eq!(matches!(result, Err(ChunkError::Size { .. })), true);
        }
    }

    #[test]
    fn test_new() {
        let data = b"greaterthanspan";
        let bmt_hash = b256!("27913f1bdb6e8e52cbd5a5fd4ab577c857287edf6969b41efe926b51de0f4f23");

        let chunk = ContentChunk::new(data.to_vec()).unwrap();
        assert_eq!(chunk.address(), bmt_hash);
        assert_eq!(chunk.data(), data.as_slice());
    }

    #[test]
    fn test_from_bytes() {
        let data = b"greaterthanspan";
        let bmt_hash = b256!("95022e6af5c6d6a564ee55a67f8455a3e18c511b5697c932d9e44f07f2fb8c53");

        let chunk = ContentChunk::try_from(data.as_slice()).unwrap();
        assert_eq!(chunk.address(), bmt_hash);
        assert_eq!(<ContentChunk as Into<Bytes>>::into(chunk), data.as_slice());
    }

    #[test]
    fn test_specific_content_hash() {
        // Test with known valid data and hash
        let data = b"foo".to_vec();
        let expected_hash =
            b256!("2387e8e7d8a48c2a9339c97c1dc3461a9a7aa07e994c5cb8b38fd7c1b3e6ea48");

        let chunk = ContentChunk::new(data).unwrap();
        assert_eq!(chunk.address(), expected_hash);

        // Test with "Digital Freedom Now"
        let data = b"Digital Freedom Now".to_vec();
        let chunk = ContentChunk::new(data).unwrap();
        assert!(chunk.address() != ChunkAddress::default()); // Ensure we get a non-zero hash
    }

    #[test]
    fn test_exact_span_size() {
        // Create a valid 8-byte span with no data
        let mut data = vec![0u8; 8];
        data.copy_from_slice(&0u64.to_le_bytes());

        let chunk = ContentChunk::try_from(data.as_slice()).unwrap();

        assert_eq!(chunk.span(), 0);
        assert_eq!(chunk.data(), &[0u8; 0].as_slice());
        assert_eq!(chunk.size(), 8);
    }
}
