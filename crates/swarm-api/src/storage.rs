//! Storage-related traits
//!
//! This module defines the traits for chunk storage in the Swarm network.

use alloc::{boxed::Box, string::String, vec::Vec};
use async_trait::async_trait;
use core::fmt::Debug;
use vertex_primitives::{ChunkAddress, Result};

use crate::{access::Credential, chunk::Chunk};

/// Storage statistics
#[derive(Debug, Clone, Default)]
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

/// Factory for creating storage implementations
#[auto_impl::auto_impl(&, Arc)]
pub trait ChunkStoreFactory: Send + Sync + 'static {
    /// The type of store this factory creates
    type Store: ChunkStore;

    /// Create a new chunk store with the given configuration
    fn create_store(&self, config: &StorageConfig) -> Result<Self::Store>;
}

/// Storage configuration
#[derive(Debug, Clone)]
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
}

/// Index for efficient chunk lookup
#[auto_impl::auto_impl(&, Arc)]
pub trait ChunkIndex: Send + Sync + 'static {
    /// Add a chunk to the index
    fn add(&self, chunk: &dyn Chunk) -> Result<()>;

    /// Remove a chunk from the index
    fn remove(&self, address: &ChunkAddress) -> Result<()>;

    /// Find chunks by a query
    fn find(&self, query: &IndexQuery) -> Result<Vec<ChunkAddress>>;

    /// Get information about a chunk
    fn get_chunk_info(&self, address: &ChunkAddress) -> Result<Option<ChunkInfo>>;
}

/// Query for the chunk index
#[derive(Debug, Clone)]
pub enum IndexQuery {
    /// Find by exact address
    ByAddress(ChunkAddress),
    /// Find chunks within a proximity range
    ByProximity {
        /// Target address
        target: ChunkAddress,
        /// Minimum proximity
        min_proximity: u8,
    },
    /// Find chunks by a custom predicate
    Custom(String),
}

/// Information about a stored chunk
#[derive(Debug, Clone)]
pub struct ChunkInfo {
    /// Chunk address
    pub address: ChunkAddress,
    /// Chunk size in bytes
    pub size: usize,
    /// Chunk type
    pub chunk_type: u8,
    /// When the chunk was stored
    pub stored_at: u64,
    /// Access count
    pub access_count: u64,
}
