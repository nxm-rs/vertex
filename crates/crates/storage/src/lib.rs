//! Storage implementations for Vertex Swarm
//!
//! This crate provides various storage backends for storing chunks,
//! including in-memory, disk-based, and database-backed implementations.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use tokio::sync::RwLock;
use tracing::{debug, error, info, trace, warn};

use vertex_primitives::{ChunkAddress, Error, Result};
use vertex_swarm_api::{
    access::Credential,
    chunk::Chunk,
    storage::{ChunkStore, StorageConfig, StorageStats},
};

mod db;
mod disk;
mod memory;

pub use db::*;
pub use disk::*;
pub use memory::*;

/// Storage error type
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// I/O error
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Database error
    #[error("Database error: {0}")]
    Database(String),

    /// Serialization error
    #[error("Serialization error: {0}")]
    Serialization(String),

    /// Chunk not found
    #[error("Chunk not found: {0}")]
    NotFound(ChunkAddress),

    /// Chunk already exists
    #[error("Chunk already exists: {0}")]
    AlreadyExists(ChunkAddress),

    /// Storage is full
    #[error("Storage is full")]
    Full,

    /// Invalid chunk
    #[error("Invalid chunk")]
    InvalidChunk,

    /// Other error
    #[error("{0}")]
    Other(String),
}

impl From<StorageError> for Error {
    fn from(err: StorageError) -> Self {
        match err {
            StorageError::Io(e) => Error::io(e.to_string()),
            StorageError::Database(e) => Error::storage(e),
            StorageError::Serialization(e) => Error::serialization(e),
            StorageError::NotFound(addr) => Error::not_found(format!("Chunk {}", addr)),
            StorageError::AlreadyExists(addr) => Error::already_exists(format!("Chunk {}", addr)),
            StorageError::Full => Error::storage("Storage is full"),
            StorageError::InvalidChunk => Error::chunk("Invalid chunk"),
            StorageError::Other(e) => Error::storage(e),
        }
    }
}

/// Helper to convert storage errors to the common Error type
pub(crate) fn storage_err<E: Into<StorageError>>(err: E) -> Error {
    Error::from(err.into())
}

/// Storage metrics
#[derive(Debug, Clone, Default)]
pub struct StorageMetrics {
    /// Total number of gets
    pub gets: u64,
    /// Total number of puts
    pub puts: u64,
    /// Total number of deletes
    pub deletes: u64,
    /// Total number of cache hits
    pub cache_hits: u64,
    /// Total number of cache misses
    pub cache_misses: u64,
}

/// A factory for creating storage backends
pub struct StorageFactory;

impl StorageFactory {
    /// Create a storage backend based on configuration
    pub fn create(config: &StorageConfig) -> Result<Arc<dyn ChunkStore>> {
        // Determine storage type from config
        let storage_type = config.storage_type.as_deref().unwrap_or("disk");

        match storage_type {
            "memory" => {
                info!("Creating in-memory chunk store");
                Ok(Arc::new(MemoryChunkStore::new()))
            }
            "disk" => {
                info!("Creating disk chunk store at {}", config.data_dir);
                let store = DiskChunkStore::new(config)?;
                Ok(Arc::new(store))
            }
            #[cfg(feature = "rocksdb")]
            "rocksdb" => {
                info!("Creating RocksDB chunk store at {}", config.data_dir);
                let store = RocksDbChunkStore::new(config)?;
                Ok(Arc::new(store))
            }
            #[cfg(feature = "sled")]
            "sled" => {
                info!("Creating Sled chunk store at {}", config.data_dir);
                let store = SledChunkStore::new(config)?;
                Ok(Arc::new(store))
            }
            _ => Err(Error::storage(format!(
                "Unsupported storage type: {}",
                storage_type
            ))),
        }
    }
}
