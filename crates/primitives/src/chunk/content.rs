use super::bmt_body::BMTBody;
use crate::chunk::error::{ChunkError, Result};
use bytes::Bytes;
use swarm_primitives_traits::{Chunk, ChunkAddress};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentChunk {
    body: BMTBody,
}

impl ContentChunk {
    /// Creates a new builder for ContentChunk
    pub fn builder() -> ContentChunkBuilder {
        ContentChunkBuilder::default()
    }

    /// Create a new ContentChunk with data (span will be inferred from data length)
    pub fn new(data: impl Into<Bytes>) -> Result<Self> {
        let data = data.into();
        Ok(Self {
            body: BMTBody::builder().data(data).build()?,
        })
    }

    /// Create a new ContentChunk with specified span and data
    pub fn new_with_span(span: u64, data: impl Into<Bytes>) -> Result<Self> {
        Ok(Self {
            body: BMTBody::builder().span(span).data(data).build()?,
        })
    }

    /// Access the chunk body's data
    pub fn data(&self) -> &[u8] {
        self.body.data()
    }

    /// Returns the span value
    pub fn span(&self) -> u64 {
        self.body.span()
    }

    /// Convert the chunk into its raw bytes representation
    pub fn into_bytes(self) -> Bytes {
        self.body.into_bytes()
    }

    /// Returns the total size of the chunk in bytes
    pub fn size(&self) -> usize {
        self.body.size()
    }
}

#[derive(Default)]
pub struct ContentChunkBuilder {
    span: Option<u64>,
    data: Option<Bytes>,
}

impl ContentChunkBuilder {
    pub fn span(mut self, span: u64) -> Self {
        self.span = Some(span);
        self
    }

    pub fn data(mut self, data: impl Into<Bytes>) -> Self {
        self.data = Some(data.into());
        self
    }

    pub fn build(self) -> Result<ContentChunk> {
        let body = BMTBody::builder()
            .span(self.span.unwrap_or_else(|| {
                self.data
                    .as_ref()
                    .map(|d| d.len() as u64)
                    .unwrap_or_default()
            }))
            .data(self.data.ok_or(ChunkError::missing_field("data"))?)
            .build()?;

        Ok(ContentChunk { body })
    }
}

impl Chunk for ContentChunk {
    fn address(&self) -> ChunkAddress {
        self.body.hash()
    }
}

impl TryFrom<&[u8]> for ContentChunk {
    type Error = ChunkError;

    fn try_from(buf: &[u8]) -> Result<Self> {
        Ok(Self {
            body: BMTBody::try_from(buf)?,
        })
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
    use alloy::primitives::b256;
    use swarm_primitives_traits::CHUNK_SIZE;

    #[test]
    fn test_builder_pattern() {
        let data = b"greaterthanspan".to_vec();
        let chunk = ContentChunk::builder().data(data.clone()).build().unwrap();

        assert_eq!(chunk.data(), &data);
        assert_eq!(chunk.span(), data.len() as u64);
    }

    #[test]
    fn test_new() {
        let data = b"greaterthanspan";
        let bmt_hash = b256!("27913f1bdb6e8e52cbd5a5fd4ab577c857287edf6969b41efe926b51de0f4f23");

        let chunk = ContentChunk::new(data.to_vec()).unwrap();
        assert_eq!(chunk.address(), bmt_hash);
        assert_eq!(chunk.data(), data);
    }

    #[test]
    fn test_new_with_span() {
        let data = b"greaterthanspan";
        let span = 42u64;
        let chunk = ContentChunk::new_with_span(span, data.to_vec()).unwrap();

        assert_eq!(chunk.data(), data);
        assert_eq!(chunk.span(), span);
    }

    #[test]
    fn test_from_bytes() {
        let data = b"greaterthanspan";
        let bmt_hash = b256!("95022e6af5c6d6a564ee55a67f8455a3e18c511b5697c932d9e44f07f2fb8c53");

        let chunk = ContentChunk::try_from(data.as_slice()).unwrap();
        assert_eq!(chunk.address(), bmt_hash);
        assert_eq!(chunk.into_bytes(), data.as_slice());
    }

    #[test]
    fn test_size_validation() {
        let result = ContentChunk::new(vec![0; CHUNK_SIZE + 1]);
        assert!(matches!(result, Err(ChunkError::Size { .. })));
    }

    #[test]
    fn test_empty_and_nil_data() {
        // Test with empty data
        let chunk = ContentChunk::new(Vec::new()).unwrap();
        assert_eq!(chunk.data().len(), 0);
        assert_eq!(chunk.span(), 0);

        // Test with zero-length slice
        let empty_slice: &[u8] = &[];
        let chunk = ContentChunk::new(empty_slice.to_vec()).unwrap();
        assert_eq!(chunk.data().len(), 0);
        assert_eq!(chunk.span(), 0);
    }

    #[test]
    fn test_invalid_chunks() {
        // Test with data exceeding chunk size
        let large_data = vec![0u8; CHUNK_SIZE + 1];
        let result = ContentChunk::new(large_data);
        assert!(matches!(result, Err(ChunkError::Size { .. })));

        // Test with invalid span size (less than 8 bytes)
        let invalid_span = vec![1, 2, 3]; // Only 3 bytes instead of required 8
        let result = ContentChunk::try_from(invalid_span.as_slice());
        assert!(matches!(result, Err(ChunkError::Size { .. })));

        // Test with span size of 7 bytes (just under required 8)
        let invalid_span = vec![1, 2, 3, 4, 5, 6, 7];
        let result = ContentChunk::try_from(invalid_span.as_slice());
        assert!(matches!(result, Err(ChunkError::Size { .. })));

        // Test with empty input
        let empty_data = vec![];
        let result = ContentChunk::try_from(empty_data.as_slice());
        assert!(matches!(result, Err(ChunkError::Size { .. })));
    }

    #[test]
    fn test_valid_chunk_verification() {
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
        assert_eq!(chunk.data(), &[0u8; 0]);
        assert_eq!(chunk.size(), 8);
    }

    #[test]
    fn test_random_sized_chunks() {
        use rand::{thread_rng, Rng};

        let mut rng = thread_rng();
        let size = rng.gen_range(1..=CHUNK_SIZE);
        let random_data = (0..size).map(|_| rng.gen::<u8>()).collect::<Vec<_>>();

        let chunk = ContentChunk::new(random_data.clone()).unwrap();
        assert_eq!(chunk.data(), random_data.as_slice());
        assert_eq!(chunk.span(), random_data.len() as u64);
    }

    #[test]
    fn test_chunk_conversion() {
        let data = b"test data".to_vec();
        let chunk = ContentChunk::new(data.clone()).unwrap();

        // Test conversion to bytes and back
        let bytes = chunk.clone().into_bytes();
        let recovered_chunk = ContentChunk::try_from(bytes.as_ref()).unwrap();

        assert_eq!(chunk.address(), recovered_chunk.address());
        assert_eq!(chunk.data(), recovered_chunk.data());
        assert_eq!(chunk.span(), recovered_chunk.span());
    }
}
