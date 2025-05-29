//! Chunk-related traits and types
//!
//! This module defines the core abstraction for data in the Swarm network.

use alloc::{boxed::Box, vec::Vec};
use core::fmt::Debug;
use vertex_primitives::{ChunkAddress, Result};

/// Chunk type identifier
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChunkType {
    /// Content-addressed chunk (default)
    ContentAddressed,
    /// Encrypted chunk
    Encrypted,
    /// Manifest chunk for organizing other chunks
    Manifest,
    /// Custom chunk type with identifier
    Custom(u8),
}

/// Core trait that all chunk types must implement
#[auto_impl::auto_impl(&, Box)]
pub trait Chunk: Send + Sync + Debug + 'static {
    /// Returns the address of the chunk
    fn address(&self) -> ChunkAddress;

    /// Returns the data contained in the chunk
    fn data(&self) -> &[u8];

    /// Returns the size of the chunk in bytes
    fn size(&self) -> usize {
        self.data().len()
    }

    /// Returns the chunk type
    fn chunk_type(&self) -> ChunkType;

    /// Verifies the integrity of the chunk
    fn verify_integrity(&self) -> bool;

    /// Clone this chunk as a boxed trait object
    fn clone_box(&self) -> Box<dyn Chunk>;
}

/// Trait for creating new chunks
#[auto_impl::auto_impl(&, Arc)]
pub trait ChunkFactory: Send + Sync + 'static {
    /// Create a new chunk from data
    fn create(&self, data: Vec<u8>) -> Result<Box<dyn Chunk>>;

    /// Create a chunk with a known address
    fn create_with_address(&self, address: ChunkAddress, data: Vec<u8>) -> Result<Box<dyn Chunk>>;

    /// Returns the type of chunks this factory creates
    fn chunk_type(&self) -> ChunkType;
}

/// A collection of chunk factories for different chunk types
#[auto_impl::auto_impl(&, Arc)]
pub trait ChunkRegistry: Send + Sync + 'static {
    /// Register a new chunk factory
    fn register_factory(&mut self, factory: Box<dyn ChunkFactory>);

    /// Get a factory for a specific chunk type
    fn factory_for(&self, chunk_type: ChunkType) -> Option<&dyn ChunkFactory>;

    /// Get the default factory
    fn default_factory(&self) -> &dyn ChunkFactory;

    /// Create a new chunk using the appropriate factory
    fn create_chunk(&self, data: Vec<u8>, chunk_type: Option<ChunkType>) -> Result<Box<dyn Chunk>>;
}
