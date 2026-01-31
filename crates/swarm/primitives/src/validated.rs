//! Type-safe validated chunk wrapper.
//!
//! [`ValidatedChunk<C>`] can only be created through validation, providing
//! compile-time guarantees that chunks have been checked against a [`ChunkTypeSet`].

use core::marker::PhantomData;

use nectar_primitives::{AnyChunk, ChunkAddress, ChunkTypeId, ChunkTypeSet};

/// Error returned when chunk validation fails.
#[derive(Debug, Clone, thiserror::Error)]
#[error("unsupported chunk type {chunk_type:?}: {reason}")]
pub struct ValidationError {
    /// The chunk type that failed validation.
    pub chunk_type: ChunkTypeId,
    /// Why validation failed.
    pub reason: &'static str,
}

/// A chunk validated against a [`ChunkTypeSet`].
///
/// This type can only be created through [`new`](Self::new), ensuring all
/// instances have been validated. The type parameter `C` specifies which
/// chunk set was used, preventing accidental use across incompatible networks.
#[derive(Debug)]
pub struct ValidatedChunk<C: ChunkTypeSet> {
    inner: AnyChunk,
    _marker: PhantomData<C>,
}

impl<C: ChunkTypeSet> Clone for ValidatedChunk<C> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _marker: PhantomData,
        }
    }
}

impl<C: ChunkTypeSet> ValidatedChunk<C> {
    /// Validate a chunk and wrap it.
    ///
    /// Returns [`ValidationError`] if the chunk's type is not supported by `C`.
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

    /// Create without validation.
    ///
    /// # Safety
    ///
    /// Caller must ensure the chunk's type is supported by `C`.
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

    /// Convert to a `ValidatedChunk` for a different chunk set.
    ///
    /// Re-validates against the target set.
    pub fn convert<D: ChunkTypeSet>(self) -> Result<ValidatedChunk<D>, ValidationError> {
        ValidatedChunk::<D>::new(self.inner)
    }
}

impl<C: ChunkTypeSet> AsRef<AnyChunk> for ValidatedChunk<C> {
    fn as_ref(&self) -> &AnyChunk {
        &self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nectar_primitives::{
        bytes::Bytes, Chunk, ContentChunk, ContentOnlyChunkSet, StandardChunkSet,
    };

    #[test]
    fn test_validated_chunk_creation() {
        let data = Bytes::from_static(b"hello world");
        let content = ContentChunk::new(data).unwrap();
        let any_chunk = AnyChunk::Content(content);

        let validated = ValidatedChunk::<StandardChunkSet>::new(any_chunk.clone());
        assert!(validated.is_ok());

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

        assert_eq!(validated.address(), &address);
        assert_eq!(validated.inner().address(), &address);
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
