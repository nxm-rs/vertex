//! Chunk-related traits and types

use crate::Result;
use core::fmt::Debug;
use vertex_primitives::{ChunkAddress, ChunkType};

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec::Vec};

/// Core chunk trait that all chunk implementations must satisfy
#[auto_impl::auto_impl(&, Arc)]
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

    /// Create a chunk with a known address (caution: may not verify integrity)
    fn create_with_address(&self, address: ChunkAddress, data: Vec<u8>) -> Result<Box<dyn Chunk>>;

    /// Returns the type of chunks this factory creates
    fn chunk_type(&self) -> ChunkType;
}

/// Trait for computing chunk addresses
#[auto_impl::auto_impl(&, Arc)]
pub trait AddressFunction: Send + Sync + 'static {
    /// Calculate the address for the given data
    fn address_of(&self, data: &[u8]) -> ChunkAddress;

    /// Get the chunk type this address function is for
    fn chunk_type(&self) -> ChunkType;
}
