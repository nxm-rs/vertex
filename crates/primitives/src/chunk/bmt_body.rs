use bytes::{Bytes, BytesMut};
use std::sync::OnceLock;
use swarm_primitives_traits::{
    chunk::{ChunkError, Result},
    ChunkAddress, ChunkBody, ChunkData, Span, CHUNK_SIZE, SEGMENT_SIZE, SPAN_SIZE,
};

use crate::bmt::HasherBuilder;

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct BMTBody {
    span: Span,
    data: Bytes,
    cached_hash: OnceLock<ChunkAddress>,
}

impl BMTBody {
    /// Creates a new builder for BMTBody
    pub fn builder() -> BMTBodyBuilder {
        BMTBodyBuilder::default()
    }

    /// Returns the span of the body
    pub(crate) fn span(&self) -> Span {
        self.span
    }

    /// Converts the body into its raw bytes representation
    // Internal method to compute the hash
    fn compute_hash(&self) -> ChunkAddress {
        let mut hasher = HasherBuilder::default()
            .build()
            .expect("Failed to create hasher");

        hasher.set_span(self.span);
        hasher.write(&self.data);

        let mut result = [0u8; SEGMENT_SIZE];
        hasher.hash(&mut result);

        result.into()
    }
}

impl ChunkData for BMTBody {
    fn data(&self) -> &[u8] {
        &self.data
    }

    fn size(&self) -> usize {
        SPAN_SIZE + self.data.len()
    }
}

impl ChunkBody for BMTBody {
    /// Returns the hash of the body, computing it if necessary
    fn hash(&self) -> ChunkAddress {
        self.cached_hash.get_or_init(|| self.compute_hash()).clone()
    }
}

impl From<BMTBody> for Bytes {
    fn from(body: BMTBody) -> Self {
        let mut bytes = BytesMut::with_capacity(body.size());
        bytes.extend_from_slice(&body.span.to_le_bytes());
        bytes.extend_from_slice(body.data());
        bytes.freeze()
    }
}

#[derive(Default)]
pub struct BMTBodyBuilder {
    span: Option<Span>,
    data: Option<Bytes>,
}

impl BMTBodyBuilder {
    pub fn span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    pub fn data(mut self, data: impl Into<Bytes>) -> Self {
        self.data = Some(data.into());
        self
    }

    pub fn build(self) -> Result<BMTBody> {
        let data = self.data.ok_or_else(|| ChunkError::missing_field("data"))?;

        // If span is not provided, use data length
        let span = self.span.unwrap_or(data.len() as u64);

        // Validate sizes
        if data.len() > CHUNK_SIZE {
            return Err(ChunkError::size(
                "data exceeds maximum chunk size",
                data.len(),
                CHUNK_SIZE,
            ));
        }

        Ok(BMTBody {
            span,
            data,
            cached_hash: OnceLock::new(),
        })
    }
}

impl TryFrom<&[u8]> for BMTBody {
    type Error = ChunkError;

    fn try_from(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < SPAN_SIZE {
            return Err(ChunkError::Size {
                context: "insufficient data for span",
                size: bytes.len(),
                limit: SPAN_SIZE,
            });
        }

        if bytes.len() > SPAN_SIZE + CHUNK_SIZE {
            return Err(ChunkError::size(
                "data exceeds maximum size",
                bytes.len(),
                SPAN_SIZE + CHUNK_SIZE,
            ));
        }

        // SAFETY: bytes.len() >= SPAN_SIZE
        let span_bytes: [u8; SPAN_SIZE] = bytes[..SPAN_SIZE].try_into().unwrap();
        let span = Span::from_le_bytes(span_bytes);
        // SAFETY: use get() to handle the case where bytes.len() == SPAN_SIZE
        let data = bytes.get(SPAN_SIZE..).unwrap_or(&[]);
        let data = Bytes::copy_from_slice(data);

        Ok(BMTBody {
            span,
            data,
            cached_hash: OnceLock::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bmt_body_creation() {
        let span = 42;
        let data = vec![1, 2, 3, 4, 5];

        let body = BMTBody::builder()
            .span(span)
            .data(data.clone())
            .build()
            .unwrap();

        assert_eq!(body.span(), span);
        assert_eq!(body.data(), &data);
    }

    #[test]
    fn test_bmt_body_from_bytes() {
        let mut input = Vec::new();
        input.extend_from_slice(&42u64.to_le_bytes()); // Span
        input.extend_from_slice(&[1, 2, 3, 4, 5]); // Data

        let body = BMTBody::try_from(input.as_slice()).unwrap();
        assert_eq!(body.span(), 42);
        assert_eq!(body.data(), &[1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_hash_caching() {
        let body = BMTBody::builder()
            .span(42)
            .data(vec![1, 2, 3])
            .build()
            .unwrap();

        let hash1 = body.hash();
        let hash2 = body.hash();
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_size_validation() {
        let result = BMTBody::builder()
            .span(42)
            .data(vec![0; CHUNK_SIZE + 1])
            .build();

        assert!(matches!(result, Err(ChunkError::Size { .. })));
    }
}
