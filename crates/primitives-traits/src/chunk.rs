use std::{io, ops::Deref, sync::OnceLock};

use crate::{ChunkAddress, BRANCHES, SEGMENT_SIZE};
use alloy_primitives::{Address, PrimitiveSignature, SignatureError};
use alloy_signer::Error as SignerError;
use bytes::Bytes;
use thiserror::Error;

pub const CHUNK_SIZE: usize = SEGMENT_SIZE * BRANCHES;

/// Common trait for accessing chunk data and size
pub trait ChunkData: Send + Sync {
    /// Get a reference to the raw data
    fn data(&self) -> &Bytes;

    /// Get the total size in bytes of the serialised chunk (including headers and other metadata)
    /// By default, just returns the size of the data.
    fn size(&self) -> usize {
        self.data().len()
    }
}

/// Core trait for all chunks
pub trait Chunk: ChunkData {
    /// Get the chunk's address/hash
    fn address(&self) -> ChunkAddress;

    /// Verify the chunk matches an expected address
    fn verify(&self, expected: ChunkAddress) -> Result<()> {
        let actual = self.address();
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

/// Trait representing the body of a chunk
pub trait ChunkBody: ChunkData {
    /// Get the hash of the body, computing it if necessary
    fn hash(&self) -> ChunkAddress;
}

/// Trait for chunks that can be signed
pub trait Signable: Chunk {
    /// Get the owner's address
    fn owner(&self) -> Address;

    /// Get the signature
    fn signature(&self) -> &PrimitiveSignature;

    /// Verify the signature
    fn verify_signature(&self) -> Result<()>;
}

/// Smart pointer wrapper for chunks with caching
pub struct CachedChunk<T: Chunk> {
    inner: T,
    cached_address: OnceLock<ChunkAddress>,
}

impl<T: Chunk> CachedChunk<T> {
    pub fn new(chunk: T) -> Self {
        Self {
            inner: chunk,
            cached_address: OnceLock::new(),
        }
    }
}

impl<T: Chunk> ChunkData for CachedChunk<T> {
    fn data(&self) -> &Bytes {
        self.inner.data()
    }

    fn size(&self) -> usize {
        self.inner.size()
    }
}

impl<T: Chunk> Chunk for CachedChunk<T> {
    fn address(&self) -> ChunkAddress {
        self.cached_address
            .get_or_init(|| self.inner.address())
            .clone()
    }
}

impl<T: Chunk> Deref for CachedChunk<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[derive(Error, Debug)]
pub enum ChunkError {
    #[error("Size error: {context} (size: {size}, limit: {limit})")]
    Size {
        context: &'static str,
        size: usize,
        limit: usize,
    },

    #[error("Invalid chunk format: {0}")]
    Format(&'static str),

    #[error("Verification failed: {context} (expected: {expected:?}, got: {got:?})")]
    Verification {
        context: &'static str,
        expected: String,
        got: String,
    },

    #[error("Crypto error: {0}")]
    Signature(#[from] SignatureError),

    #[error("Signer error: {0}")]
    Signer(#[from] SignerError),

    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("Missing required field: {0}")]
    MissingField(&'static str),

    #[error("Decoding error: {0}")]
    Decode(#[from] std::array::TryFromSliceError),
}

// Type alias for Result
pub type Result<T> = std::result::Result<T, ChunkError>;

// Helper methods for error creation
impl ChunkError {
    pub fn size(context: &'static str, size: usize, limit: usize) -> Self {
        Self::Size {
            context,
            size,
            limit,
        }
    }

    pub fn format(msg: &'static str) -> Self {
        Self::Format(msg)
    }

    pub fn verification<T: std::fmt::Debug>(context: &'static str, expected: T, got: T) -> Self {
        Self::Verification {
            context,
            expected: format!("{:?}", expected),
            got: format!("{:?}", got),
        }
    }

    pub fn missing_field(field: &'static str) -> Self {
        Self::MissingField(field)
    }

    pub fn decode(err: std::array::TryFromSliceError) -> Self {
        Self::Decode(err)
    }
}
