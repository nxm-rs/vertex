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
