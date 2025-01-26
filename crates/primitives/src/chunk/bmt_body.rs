use bytes::{Bytes, BytesMut};
use std::sync::OnceLock;
use swarm_primitives_traits::{ChunkAddress, ChunkBody, Span, CHUNK_SIZE, SEGMENT_SIZE, SPAN_SIZE};
use thiserror::Error;

use crate::bmt::HasherBuilder;

#[derive(Error, Debug)]
pub enum BMTBodyError {
    #[error("Data size {size} exceeds maximum {max}")]
    SizeExceeded { size: usize, max: usize },

    #[error("Data size {size} below required minimum {min}")]
    InsufficientSize { size: usize, min: usize },

    #[error("Invalid span encoding")]
    InvalidSpan,

    #[error("Missing required field: {0}")]
    MissingField(&'static str),
}

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
    pub fn span(&self) -> Span {
        self.span
    }

    /// Returns a reference to the data
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Returns the hash of the body, computing it if necessary
    pub fn hash(&self) -> ChunkAddress {
        self.cached_hash.get_or_init(|| self.compute_hash()).clone()
    }

    /// Converts the body into its raw bytes representation
    pub fn into_bytes(self) -> Bytes {
        let mut bytes = BytesMut::with_capacity(SPAN_SIZE + self.data.len());
        bytes.extend_from_slice(&self.span.to_le_bytes());
        bytes.extend_from_slice(&self.data);
        bytes.freeze()
    }

    /// Returns the total size of the body in bytes
    pub fn size(&self) -> usize {
        SPAN_SIZE + self.data.len()
    }

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

    pub fn build(self) -> Result<BMTBody, BMTBodyError> {
        let data = self.data.ok_or(BMTBodyError::MissingField("data"))?;

        // If span is not provided, use data length
        let span = self.span.unwrap_or(data.len() as u64);

        // Validate sizes
        if data.len() > CHUNK_SIZE {
            return Err(BMTBodyError::SizeExceeded {
                size: data.len(),
                max: CHUNK_SIZE,
            });
        }

        Ok(BMTBody {
            span,
            data,
            cached_hash: OnceLock::new(),
        })
    }
}

impl TryFrom<&[u8]> for BMTBody {
    type Error = BMTBodyError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        if bytes.len() < SPAN_SIZE {
            return Err(BMTBodyError::InsufficientSize {
                size: bytes.len(),
                min: SPAN_SIZE,
            });
        }

        if bytes.len() > SPAN_SIZE + CHUNK_SIZE {
            return Err(BMTBodyError::SizeExceeded {
                size: bytes.len(),
                max: SPAN_SIZE + CHUNK_SIZE,
            });
        }

        let span_bytes: [u8; SPAN_SIZE] = bytes[..SPAN_SIZE]
            .try_into()
            .map_err(|_| BMTBodyError::InvalidSpan)?;

        let span = Span::from_le_bytes(span_bytes);
        // Use get() to safely handle the case where bytes.len() == SPAN_SIZE
        let data = bytes.get(SPAN_SIZE..).unwrap_or(&[]);
        let data = Bytes::copy_from_slice(data);

        Ok(BMTBody {
            span,
            data,
            cached_hash: OnceLock::new(),
        })
    }
}

impl ChunkBody for BMTBody {
    fn hash(&self) -> ChunkAddress {
        BMTBody::hash(self)
    }

    fn data(&self) -> &[u8] {
        self.data()
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

        assert!(matches!(result, Err(BMTBodyError::SizeExceeded { .. })));
    }
}
