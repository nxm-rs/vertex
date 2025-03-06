//! Error types for the primitives crate.

use thiserror::Error;

/// Generic result type for operations in this crate
pub type Result<T> = std::result::Result<T, Error>;

/// Primary error type for the primitives crate
#[derive(Error, Debug)]
pub enum Error {
    /// Errors related to chunk operations
    #[error(transparent)]
    Chunk(#[from] crate::chunk::error::ChunkError),

    /// Errors related to storage operations
    #[error(transparent)]
    Storage(#[from] crate::storage::error::StorageError),

    /// Errors related to digest operations
    #[error(transparent)]
    Digest(#[from] crate::bmt::error::DigestError),

    /// Errors related to access control
    #[error(transparent)]
    AccessControl(#[from] nectar_access_control::Error),

    /// I/O errors
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Other errors
    #[error("{0}")]
    Other(String),
}

impl Error {
    /// Create a new generic error
    pub fn other<S: Into<String>>(msg: S) -> Self {
        Self::Other(msg.into())
    }
}

/// Re-export submodule errors
pub use crate::bmt::error::DigestError;
pub use crate::chunk::error::ChunkError;
pub use crate::storage::error::StorageError;
