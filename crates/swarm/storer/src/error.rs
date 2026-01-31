//! Storer error types.

use nectar_primitives::ChunkAddress;

/// Errors from storer operations.
#[derive(Debug, thiserror::Error)]
pub enum StorerError {
    /// Database error.
    #[error("database error: {0}")]
    Database(String),

    /// Chunk not found.
    #[error("chunk not found: {0}")]
    NotFound(ChunkAddress),

    /// Storage full.
    #[error("storage full: capacity {capacity}, used {used}")]
    StorageFull { capacity: u64, used: u64 },

    /// Invalid chunk data.
    #[error("invalid chunk: {0}")]
    InvalidChunk(String),

    /// Serialization error.
    #[error("serialization error: {0}")]
    Serialization(String),

    /// IO error.
    #[error("io error: {0}")]
    Io(String),
}

impl From<redb::DatabaseError> for StorerError {
    fn from(err: redb::DatabaseError) -> Self {
        StorerError::Database(err.to_string())
    }
}

impl From<redb::TransactionError> for StorerError {
    fn from(err: redb::TransactionError) -> Self {
        StorerError::Database(err.to_string())
    }
}

impl From<redb::TableError> for StorerError {
    fn from(err: redb::TableError) -> Self {
        StorerError::Database(err.to_string())
    }
}

impl From<redb::StorageError> for StorerError {
    fn from(err: redb::StorageError) -> Self {
        StorerError::Database(err.to_string())
    }
}

impl From<redb::CommitError> for StorerError {
    fn from(err: redb::CommitError) -> Self {
        StorerError::Database(err.to_string())
    }
}
