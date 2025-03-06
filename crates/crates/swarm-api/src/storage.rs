//! Storage-related traits

use crate::{Chunk, Credential, Result};
use vertex_primitives::ChunkAddress;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::String, vec::Vec};
use core::fmt::Debug;

/// Storage statistics
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct StorageStats {
    /// Total number of chunks stored
    pub total_chunks: usize,
    /// Total storage space used in bytes
    pub used_space: u64,
    /// Total storage space available in bytes
    pub available_space: u64,
    /// Storage utilization percentage (0-100)
    pub utilization_percent: f32,
}

/// Core storage trait for chunk persistence
#[auto_impl::auto_impl(&, Arc)]
pub trait ChunkStore: Send + Sync + 'static {
    /// Store a chunk with its associated credential
    fn put(&self, chunk: Box<dyn Chunk>, credential: &dyn Credential) -> Result<()>;

    /// Retrieve a chunk by its address
    fn get(&self, address: &ChunkAddress) -> Result<Option<Box<dyn Chunk>>>;

    /// Check if a chunk exists in the store
    fn contains(&self, address: &ChunkAddress) -> Result<bool>;

    /// Delete a chunk from the store
    fn delete(&self, address: &ChunkAddress) -> Result<()>;

    /// Return the number of chunks in the store
    fn len(&self) -> Result<usize>;

    /// Check if the store is empty
    fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Get statistics about the store
    fn stats(&self) -> Result<StorageStats>;

    /// Iterate over all chunks in the store
    fn iter(&self) -> Box<dyn Iterator<Item = Result<Box<dyn Chunk>>> + '_>;
}

/// Storage configuration
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct StorageConfig {
    /// Root directory for storage
    pub root_dir: String,
    /// Maximum storage space in bytes
    pub max_space: u64,
    /// Target storage space in bytes
    pub target_space: u64,
    /// Minimum chunk size in bytes
    pub min_chunk_size: usize,
    /// Maximum chunk size in bytes
    pub max_chunk_size: usize,
    /// Whether to validate chunks on read
    pub validate_on_read: bool,
    /// Cache capacity in number of chunks
    pub cache_capacity: usize,
    /// Whether to persist storage metadata
    pub persist_metadata: bool,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            root_dir: "./storage".into(),
            max_space: 1024 * 1024 * 1024,   // 1GB
            target_space: 512 * 1024 * 1024, // 512MB
            min_chunk_size: 1,
            max_chunk_size: 4 * 1024 * 1024, // 4MB
            validate_on_read: true,
            cache_capacity: 1000,
            persist_metadata: true,
        }
    }
}

/// Factory for creating storage implementations
#[auto_impl::auto_impl(&, Arc)]
pub trait ChunkStoreFactory: Send + Sync + 'static {
    /// Create a new chunk store with the given configuration
    fn create_store(&self, config: &StorageConfig) -> Result<Box<dyn ChunkStore>>;
}
