//! Storer error types.

use nectar_primitives::ChunkAddress;
use vertex_storage::DatabaseError;

/// Errors from storer operations.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum StorerError {
    /// Database error.
    #[error("database error: {0}")]
    Database(#[from] DatabaseError),

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
    Serialization(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// IO error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl StorerError {
    vertex_metrics::impl_record_error!("storer_errors_total");
}

impl From<redb::DatabaseError> for StorerError {
    fn from(err: redb::DatabaseError) -> Self {
        StorerError::Database(DatabaseError::open_err(err))
    }
}

impl From<redb::TransactionError> for StorerError {
    fn from(err: redb::TransactionError) -> Self {
        StorerError::Database(DatabaseError::InitTx(
            vertex_storage::DatabaseErrorInfo::from_err(err),
        ))
    }
}

impl From<redb::TableError> for StorerError {
    fn from(err: redb::TableError) -> Self {
        StorerError::Database(DatabaseError::Read(
            vertex_storage::DatabaseErrorInfo::from_err(err),
        ))
    }
}

impl From<redb::StorageError> for StorerError {
    fn from(err: redb::StorageError) -> Self {
        StorerError::Database(DatabaseError::Read(
            vertex_storage::DatabaseErrorInfo::from_err(err),
        ))
    }
}

impl From<redb::CommitError> for StorerError {
    fn from(err: redb::CommitError) -> Self {
        StorerError::Database(DatabaseError::commit_err(err))
    }
}
