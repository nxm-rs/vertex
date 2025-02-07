//! The `chunk::BMTBody` module provides data structures and utilities for managing Binary Merkle Tree (BMT) bodies in
//! a decentralised storage network context. A BMT body represents a unit of data with specific characteristics such
//! as span, and data content.
//!
//! This module includes:
//!
//! - The `BMTBody` struct: represents a BMT body with properties like span, and data content.
//! - A builder pattern (`BMTBodyBuilder`): Facilitates the creation of BMT bodies for setting various parameters in
//!   a structured manner.
use bytes::{Bytes, BytesMut};
use nectar_primitives_traits::{
    chunk::{ChunkError, Result},
    ChunkAddress, ChunkBody, ChunkData, Span, CHUNK_SIZE, SPAN_SIZE,
};
use std::{marker::PhantomData, sync::OnceLock};

use crate::bmt::HasherBuilder;

/// Represents a Binary Merkle Tree (BMT) body in a decentralised storage network context.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct BMTBody {
    /// The span of the BMT body which represents the length of the data content in bytes
    /// as the data may in itself span multiple chunks.
    span: Span,
    /// The raw data content of the BMT body.
    data: Bytes,
    /// Cache the hash of the BMT body for efficient retrieval.
    cached_hash: OnceLock<ChunkAddress>,
}

impl BMTBody {
    // Zero-copy constructor
    fn new_unchecked(span: Span, data: Bytes) -> Self {
        Self {
            span,
            data,
            cached_hash: OnceLock::new(),
        }
    }
    /// Creates a new builder for `BMTBody`
    pub fn builder() -> BMTBodyBuilder<Initial, BMTBody> {
        BMTBodyBuilder::default()
    }

    /// Returns the span of the body
    pub(crate) fn span(&self) -> Span {
        self.span
    }

    // Internal method to compute the hash
    fn hash(&self) -> ChunkAddress {
        let mut hasher = HasherBuilder::default()
            .build()
            .expect("Failed to create hasher");

        hasher.set_span(self.span);
        hasher.write(self.data.as_ref());

        let mut result = ChunkAddress::default();
        hasher.hash(result.as_mut());
        result
    }
}

// Validates the data size and returns the data as `Bytes`.
fn validate_data(data: impl Into<Bytes>) -> Result<Bytes> {
    let data = data.into();
    if data.len() > CHUNK_SIZE {
        return Err(ChunkError::size(
            "data exceeds maximum chunk size",
            data.len(),
            CHUNK_SIZE,
        ));
    }
    Ok(data)
}

impl ChunkData for BMTBody {
    fn data(&self) -> &Bytes {
        &self.data
    }

    fn size(&self) -> usize {
        SPAN_SIZE + self.data.len()
    }
}

impl ChunkBody for BMTBody {
    fn hash(&self) -> ChunkAddress {
        self.cached_hash.get_or_init(|| self.hash()).clone()
    }
}

impl From<BMTBody> for Bytes {
    fn from(body: BMTBody) -> Self {
        let mut bytes = BytesMut::with_capacity(body.size());
        bytes.extend_from_slice(&body.span.to_le_bytes());
        bytes.extend_from_slice(body.data().as_ref());
        bytes.freeze()
    }
}

/// Marker traits for builder states
pub trait BuilderState {}

#[derive(Default)]
pub struct Initial;
impl BuilderState for Initial {}

/// State of the BMTBody builder after span has been set.
pub struct WithSpan;
impl BuilderState for WithSpan {}

/// State of the BMTBody builder when all fields are set.
pub struct ReadyToBuild;
impl BuilderState for ReadyToBuild {}

/// A stateful builder for creating `BMTBody` instances.
///
/// This builder pattern ensures that all required fields are properly configured before building a `BMTBody`.
/// It enforces the following sequence of states:
/// 1. Initial (no fields set)
/// 2. WithSpan (span is set)
/// 3. ReadyToBuild (all fields are set)
pub struct BMTBodyBuilder<S: BuilderState, T = BMTBody>
where
    T: From<BMTBody>,
{
    config: BMTBodyConfig,
    _state: PhantomData<S>,
    _output: PhantomData<T>,
}

impl<T: From<BMTBody>> Default for BMTBodyBuilder<Initial, T> {
    fn default() -> Self {
        Self {
            config: BMTBodyConfig::default(),
            _state: PhantomData,
            _output: PhantomData,
        }
    }
}

#[derive(Default)]
pub struct BMTBodyConfig {
    /// The span of the BMT body.
    span: Option<Span>,
    /// The raw data content of the BMT body.
    data: Option<Bytes>,
}

impl<S: BuilderState> BMTBodyBuilder<S> {}

impl<T: From<BMTBody>> BMTBodyBuilder<Initial, T> {
    /// Sets the span and transitions to the `WithSpan` state.
    pub fn with_span(mut self, span: u64) -> BMTBodyBuilder<WithSpan, T> {
        self.config.span = Some(span);
        BMTBodyBuilder {
            config: self.config,
            _state: PhantomData,
            _output: PhantomData,
        }
    }

    /// Sets the data automatically from the input and transitions to the `ReadyToBuild` state.
    /// Ensures that the data size does not exceed the maximum chunk size.
    pub fn auto_from_data(
        mut self,
        data: impl Into<Bytes>,
    ) -> Result<BMTBodyBuilder<ReadyToBuild, T>> {
        self.config.data = Some(validate_data(data)?);

        // Automatically set the span based on the data length
        self.config.span = Some(self.config.data.as_ref().unwrap().len() as u64);

        Ok(BMTBodyBuilder {
            config: self.config,
            _state: PhantomData,
            _output: PhantomData,
        })
    }
}

impl<T: From<BMTBody>> BMTBodyBuilder<WithSpan, T> {
    /// Sets the data and transitions to the `ReadyToBuild` state.
    /// Ensures that the data size does not exceed the maximum chunk size.
    pub fn with_data(mut self, data: impl Into<Bytes>) -> Result<BMTBodyBuilder<ReadyToBuild, T>> {
        self.config.data = Some(validate_data(data)?);

        let span = self.config.span.unwrap();
        if span <= CHUNK_SIZE as u64 && self.config.data.as_ref().unwrap().len() != span as usize {
            return Err(ChunkError::Size {
                context: "span does not match data size",
                size: self.config.data.as_ref().unwrap().len(),
                limit: span as usize,
            });
        }

        Ok(BMTBodyBuilder {
            config: self.config,
            _state: PhantomData,
            _output: PhantomData,
        })
    }
}

impl<T: From<BMTBody>> BMTBodyBuilder<ReadyToBuild, T> {
    /// Builds a new BMTBody instance.
    pub fn build(self) -> Result<T> {
        // SAFETY: span and data are enforced to be Some in the ReadyToBuild state
        let span = self.config.span.unwrap();
        let data = self.config.data.unwrap();

        let bmt_body = BMTBody::new_unchecked(span, data);
        Ok(T::from(bmt_body))
    }
}

impl TryFrom<Bytes> for BMTBody {
    type Error = ChunkError;

    /// Tries to create a `BMTBody` instance from raw bytes.
    fn try_from(mut buf: Bytes) -> Result<Self> {
        if buf.len() < SPAN_SIZE {
            return Err(ChunkError::Size {
                context: "insufficient data for span",
                size: buf.len(),
                limit: SPAN_SIZE,
            });
        }

        // SAFETY: bytes.len() >= SPAN_SIZE
        let span = Span::from_le_bytes(buf.split_to(SPAN_SIZE).as_ref().try_into()?);
        Ok(BMTBody::builder().with_span(span).with_data(buf)?.build()?)
    }
}

impl TryFrom<&[u8]> for BMTBody {
    type Error = ChunkError;

    fn try_from(buf: &[u8]) -> Result<Self> {
        Self::try_from(Bytes::copy_from_slice(buf))
    }
}

impl<'a> arbitrary::Arbitrary<'a> for BMTBody {
    fn arbitrary(u: &mut arbitrary::Unstructured<'a>) -> arbitrary::Result<Self> {
        // Generate a random span value
        let span = Span::arbitrary(u)?;

        // Ensure data size does not exceed CHUNK_SIZE
        let data_len: usize = u.int_in_range(0..=CHUNK_SIZE as usize)?;
        let mut buf = vec![0; data_len];
        u.fill_buffer(&mut buf)?;

        // Convert buffer to Bytes
        let data = Bytes::from(buf);

        Ok(BMTBodyBuilder::default()
            .with_span(span)
            .with_data(data)
            .unwrap()
            .build()
            .unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use proptest_arbitrary_interop::arb;

    // Define strategies for generating BMTBody using the Arbitrary implementation
    fn bmt_body_strategy() -> impl Strategy<Value = BMTBody> {
        arb::<BMTBody>()
    }

    fn create_bmt_body(span: u64, data: Vec<u8>) -> Result<BMTBody> {
        BMTBody::builder().with_span(span).with_data(data)?.build()
    }

    proptest! {
        #[test]
        fn test_bmt_body_properties(body in bmt_body_strategy()) {
            // Test that span is within valid range
            prop_assert!(body.span() <= u64::MAX);

            // Test that data size is within valid range
            prop_assert!(body.data().len() <= CHUNK_SIZE);

            // Test that total size is correct
            prop_assert_eq!(body.size(), SPAN_SIZE + body.data().len());

            // Test serialisation / deserialisation
            let bytes: Bytes = body.clone().into();
            let decoded = BMTBody::try_from(bytes).unwrap();
            prop_assert_eq!(body, decoded);
        }

        #[test]
        fn test_bmt_body_size_validation(span in 0..=u64::MAX, data_len in CHUNK_SIZE + 1..=CHUNK_SIZE * 2) {
            let data = vec![0; data_len];
            let result = create_bmt_body(span, data);
            assert!(matches!(result, Err(ChunkError::Size { .. })));
        }

        #[test]
        fn test_bmt_body_builder_properties(
            span in 0..=u64::MAX,
            data_len in 0..=CHUNK_SIZE,
        ) {
            let data = vec![0; data_len];
            let builder = BMTBodyBuilder::default()
                .with_span(span)
                .with_data(data.clone())?;

            let body: BMTBody = builder.build().unwrap();
            assert_eq!(body.span(), span);
            assert_eq!(body.data(), &data);
            prop_assert_eq!(body.size(), SPAN_SIZE + data.len());
        }

        #[test]
        fn test_span_data_length_mismatch(
            span in 0..=CHUNK_SIZE as u64,
            data_len in 0..=CHUNK_SIZE,
        ) {
            let data = vec![0; data_len];
            let result = BMTBody::builder()
                .with_span(span)
                .with_data(data.clone());

            if span <= CHUNK_SIZE as u64 && data.len() != span as usize {
                assert!(matches!(result, Err(ChunkError::Size { .. })));
            } else {
                assert!(matches!(result, Ok(_)));
            }
        }
    }

    #[test]
    fn test_bmt_body_creation() {
        let span = 5;
        let data = vec![1, 2, 3, 4, 5];
        let body = create_bmt_body(span, data.clone()).unwrap();

        assert_eq!(body.span(), span);
        assert_eq!(body.data(), &data);
        assert_eq!(body.size(), SPAN_SIZE + data.len());
    }

    #[test]
    fn test_bmt_body_from_bytes() {
        let mut input = Vec::new();
        input.extend_from_slice(&5u64.to_le_bytes()); // Span
        input.extend_from_slice(&[1, 2, 3, 4, 5]); // Data

        let body = BMTBody::try_from(Bytes::from(input)).unwrap();
        assert_eq!(body.span(), 5);
        assert_eq!(body.data(), &[1, 2, 3, 4, 5].as_slice());
    }

    #[test]
    fn test_hash_caching() {
        let body = create_bmt_body(3, vec![1, 2, 3]).unwrap();

        let hash1 = body.hash();
        let hash2 = body.hash();
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_size_validation() {
        let result = BMTBody::builder()
            .with_span(42)
            .with_data(vec![0; CHUNK_SIZE + 1]);

        assert!(matches!(result, Err(ChunkError::Size { .. })));

        let result = BMTBody::try_from(vec![0; CHUNK_SIZE + 9].as_slice());
        assert!(matches!(result, Err(ChunkError::Size { .. })));
    }
}
