//! Validated chunk types
//!
//! This module provides [`ValidatedChunk`], a wrapper type that proves a chunk
//! has been validated against a [`ChunkTypeSet`]. This enables compile-time
//! enforcement of validation requirements.
//!
//! # Design
//!
//! `ValidatedChunk<C>` can only be created through validation, making it
//! impossible to accidentally use an unvalidated chunk where validation is
//! required. The type parameter `C` specifies which [`ChunkTypeSet`] the chunk
//! was validated against.
//!
//! # Example
//!
//! ```ignore
//! use vertex_primitives::{ValidatedChunk, AnyChunk, StandardChunkSet};
//!
//! // Create a validated chunk (validates on construction)
//! let chunk: AnyChunk = /* ... */;
//! let validated = ValidatedChunk::<StandardChunkSet>::new(chunk)?;
//!
//! // Use the validated chunk - type system guarantees it's valid
//! store.put(validated, credential)?;
//! ```

use core::marker::PhantomData;

use crate::{AnyChunk, ChunkAddress, ChunkTypeId, ChunkTypeSet};

// ============================================================================
// ValidationError
// ============================================================================

/// Error returned when chunk validation fails.
///
/// Contains the chunk type that was rejected and a description of why.
#[derive(Debug, Clone, thiserror::Error)]
#[error("unsupported chunk type {chunk_type:?}: {reason}")]
pub struct ValidationError {
    /// The chunk type that failed validation.
    pub chunk_type: ChunkTypeId,
    /// Description of why validation failed.
    pub reason: &'static str,
}

/// A chunk that has been validated against a [`ChunkTypeSet`].
///
/// This type can only be created through the [`new`](Self::new) method, which
/// validates the chunk. This provides compile-time guarantees that functions
/// accepting `ValidatedChunk<C>` receive only valid chunks.
///
/// # Type Parameter
///
/// The `C` parameter specifies which [`ChunkTypeSet`] this chunk was validated
/// against. This ensures that chunks validated for one network can't be
/// accidentally used on a network with different supported types.
///
/// # Zero-Cost Abstraction
///
/// When validation succeeds, `ValidatedChunk` is just a newtype wrapper around
/// `AnyChunk` with no runtime overhead for accessing the inner chunk.
#[derive(Debug)]
pub struct ValidatedChunk<C: ChunkTypeSet> {
    inner: AnyChunk,
    _marker: PhantomData<C>,
}

// Manual Clone impl to avoid requiring C: Clone (PhantomData doesn't need it)
impl<C: ChunkTypeSet> Clone for ValidatedChunk<C> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _marker: PhantomData,
        }
    }
}

impl<C: ChunkTypeSet> ValidatedChunk<C> {
    /// Validate a chunk and wrap it in `ValidatedChunk`.
    ///
    /// This is the only way to create a `ValidatedChunk`, ensuring that all
    /// instances have been validated.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError`] if the chunk's type is not supported by `C`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let validated = ValidatedChunk::<StandardChunkSet>::new(chunk)?;
    /// ```
    pub fn new(chunk: AnyChunk) -> Result<Self, ValidationError> {
        if !C::supports(chunk.type_id()) {
            return Err(ValidationError {
                chunk_type: chunk.type_id(),
                reason: "chunk type not supported by this chunk set",
            });
        }
        Ok(Self {
            inner: chunk,
            _marker: PhantomData,
        })
    }

    /// Create a `ValidatedChunk` without validation.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the chunk's type is supported by `C`.
    /// Using this incorrectly can lead to invalid chunks being treated as valid.
    ///
    /// This is useful when you know the chunk is valid (e.g., it was just
    /// retrieved from storage where it was validated on insert).
    #[inline]
    pub unsafe fn new_unchecked(chunk: AnyChunk) -> Self {
        debug_assert!(
            C::supports(chunk.type_id()),
            "chunk type {:?} not supported by chunk set",
            chunk.type_id()
        );
        Self {
            inner: chunk,
            _marker: PhantomData,
        }
    }

    /// Get a reference to the inner chunk.
    #[inline]
    pub fn inner(&self) -> &AnyChunk {
        &self.inner
    }

    /// Get the chunk's address.
    #[inline]
    pub fn address(&self) -> &ChunkAddress {
        self.inner.address()
    }

    /// Get the chunk's type ID.
    #[inline]
    pub fn type_id(&self) -> ChunkTypeId {
        self.inner.type_id()
    }

    /// Consume and return the inner chunk.
    #[inline]
    pub fn into_inner(self) -> AnyChunk {
        self.inner
    }

    /// Convert to a `ValidatedChunk` for a different (compatible) chunk set.
    ///
    /// This re-validates the chunk against the new chunk set.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError`] if the chunk's type is not supported by the target set.
    pub fn convert<D: ChunkTypeSet>(self) -> Result<ValidatedChunk<D>, ValidationError> {
        ValidatedChunk::<D>::new(self.inner)
    }
}

impl<C: ChunkTypeSet> AsRef<AnyChunk> for ValidatedChunk<C> {
    fn as_ref(&self) -> &AnyChunk {
        &self.inner
    }
}

// NOTE: We intentionally do NOT implement Deref to AnyChunk.
// Deref causes method resolution issues where .clone() would resolve to
// AnyChunk::clone() instead of ValidatedChunk::clone(), breaking type safety.
// Use .inner() or .as_ref() to access the underlying AnyChunk when needed.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Chunk, ContentChunk, ContentOnlyChunkSet, StandardChunkSet};
    use nectar_primitives::bytes::Bytes;

    #[test]
    fn test_validated_chunk_creation() {
        let data = Bytes::from_static(b"hello world");
        let content = ContentChunk::new(data).unwrap();
        let any_chunk = AnyChunk::Content(content);

        // Should succeed for StandardChunkSet (supports CAC)
        let validated = ValidatedChunk::<StandardChunkSet>::new(any_chunk.clone());
        assert!(validated.is_ok());

        // Should also succeed for ContentOnlyChunkSet
        let validated = ValidatedChunk::<ContentOnlyChunkSet>::new(any_chunk);
        assert!(validated.is_ok());
    }

    #[test]
    fn test_validated_chunk_access() {
        let data = Bytes::from_static(b"test data");
        let content = ContentChunk::new(data).unwrap();
        let address = *content.address();
        let any_chunk = AnyChunk::Content(content);

        let validated = ValidatedChunk::<StandardChunkSet>::new(any_chunk).unwrap();

        // Can access inner chunk
        assert_eq!(validated.address(), &address);
        assert_eq!(validated.inner().address(), &address);

        // Can access as AnyChunk via inner() or as_ref()
        let _: &AnyChunk = validated.inner();
        let _: &AnyChunk = validated.as_ref();
    }

    #[test]
    fn test_validated_chunk_into_inner() {
        let data = Bytes::from_static(b"test");
        let content = ContentChunk::new(data).unwrap();
        let any_chunk = AnyChunk::Content(content);

        let validated = ValidatedChunk::<StandardChunkSet>::new(any_chunk.clone()).unwrap();
        let recovered = validated.into_inner();

        assert_eq!(recovered.address(), any_chunk.address());
    }
}
