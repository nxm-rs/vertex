//! Core chunk types and traits.
//!
//! Chunks are the fundamental unit of data in the storage system.

mod address;
pub mod content_addressed;
pub mod custom;
pub mod error;
pub mod single_owner;

pub use address::ChunkAddress;
pub use content_addressed::ContentAddressedChunk;
pub use custom::{register_custom_deserializer, CustomChunk};
pub use error::{ChunkError, Result as ChunkResult};
pub use single_owner::SingleOwnerChunk;

use crate::error::Result; // Use crate-level Result for ChunkData methods
use bytes::{BufMut, Bytes, BytesMut};

/// Possible chunk types in the network
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChunkType {
    /// Standard content-addressed chunk (default)
    ContentAddressed,
    /// Single-owner chunk with custom addressing
    SingleOwner,
    /// Custom chunk type with identifier (0xE0-0xEF)
    Custom(u8),
}

impl ChunkType {
    /// Convert chunk type to byte identifier
    pub const fn to_byte(&self) -> u8 {
        match self {
            Self::ContentAddressed => 0,
            Self::SingleOwner => 1,
            Self::Custom(id) => *id,
        }
    }

    /// Create chunk type from byte identifier
    pub const fn from_byte(byte: u8) -> Self {
        match byte {
            0 => Self::ContentAddressed,
            1 => Self::SingleOwner,
            id if id >= 0xE0 && id <= 0xEF => Self::Custom(id),
            id => Self::Custom(id),
        }
    }

    /// Check if a type ID is valid for custom chunks
    pub fn is_valid_custom_type(type_id: u8) -> bool {
        type_id >= 0xE0 && type_id <= 0xEF
    }
}

/// Main chunk data enum that represents all possible chunk types
#[derive(Clone)]
pub enum ChunkData {
    /// Content-addressed chunk (handles its own versioning)
    ContentAddressed(ContentAddressedChunk),

    /// Single-owner chunk (handles its own versioning)
    SingleOwner(SingleOwnerChunk),

    /// Custom chunk type (ID 0xE0-0xEF)
    Custom(Box<dyn CustomChunk>),
}

impl ChunkData {
    /// Get the chunk's address/hash
    pub fn address(&self) -> ChunkAddress {
        match self {
            Self::ContentAddressed(chunk) => chunk.address(),
            Self::SingleOwner(chunk) => chunk.address(),
            Self::Custom(chunk) => chunk.address(),
        }
    }

    /// Get the type of the chunk
    pub fn chunk_type(&self) -> ChunkType {
        match self {
            Self::ContentAddressed(_) => ChunkType::ContentAddressed,
            Self::SingleOwner(_) => ChunkType::SingleOwner,
            Self::Custom(chunk) => ChunkType::Custom(chunk.type_id()),
        }
    }

    /// Get the version of the chunk format
    pub fn version(&self) -> u8 {
        match self {
            Self::ContentAddressed(chunk) => chunk.version(),
            Self::SingleOwner(chunk) => chunk.version(),
            Self::Custom(chunk) => chunk.version(),
        }
    }

    /// Get the fixed size of the header for this chunk type and version
    pub fn header_size(&self) -> usize {
        match self {
            Self::ContentAddressed(chunk) => chunk.header_size(),
            Self::SingleOwner(chunk) => chunk.header_size(),
            Self::Custom(chunk) => chunk.header().len(),
        }
    }

    /// Get the chunk's header based on its type
    pub fn header(&self) -> &[u8] {
        match self {
            Self::ContentAddressed(chunk) => chunk.header(),
            Self::SingleOwner(chunk) => chunk.header(),
            Self::Custom(chunk) => chunk.header(),
        }
    }

    /// Get the chunk's payload (data excluding header)
    pub fn payload(&self) -> &[u8] {
        match self {
            Self::ContentAddressed(chunk) => chunk.payload(),
            Self::SingleOwner(chunk) => chunk.payload(),
            Self::Custom(chunk) => chunk.payload(),
        }
    }

    /// Get the complete raw data
    pub fn data(&self) -> &[u8] {
        match self {
            Self::ContentAddressed(chunk) => chunk.data(),
            Self::SingleOwner(chunk) => chunk.data(),
            Self::Custom(chunk) => chunk.data(),
        }
    }

    /// Get the total size in bytes of the chunk
    pub fn size(&self) -> usize {
        self.data().len()
    }

    /// Verify the integrity of the chunk
    pub fn verify_integrity(&self) -> Result<()> {
        match self {
            Self::ContentAddressed(chunk) => chunk.verify_integrity(),
            Self::SingleOwner(chunk) => chunk.verify_integrity(),
            Self::Custom(chunk) => chunk.verify_integrity(),
        }
    }

    /// Verify the chunk matches an expected address
    pub fn verify(&self, expected: ChunkAddress) -> Result<()> {
        let actual = self.address();
        if actual != expected {
            return Err(ChunkError::verification("address mismatch", expected, actual).into());
        }

        self.verify_integrity()
    }

    /// Serialize the chunk to bytes efficiently
    pub fn serialize(&self, with_type_prefix: bool) -> Bytes {
        let prefix_len = if with_type_prefix { 2 } else { 0 };
        let mut buffer = BytesMut::with_capacity(prefix_len + self.size());

        if with_type_prefix {
            buffer.put_u8(self.chunk_type().to_byte());
            buffer.put_u8(self.version());
        }

        buffer.extend_from_slice(self.data());

        buffer.freeze()
    }

    /// Deserialize bytes into a chunk
    pub fn deserialize(data: Bytes, has_type_prefix: bool) -> Result<Self> {
        if has_type_prefix {
            if data.len() < 2 {
                return Err(ChunkError::format("Data too short for type prefix").into());
            }

            let type_byte = data[0];
            let version = data[1];
            let data_without_prefix = data.slice(2..);

            match ChunkType::from_byte(type_byte) {
                ChunkType::ContentAddressed => {
                    let chunk = ContentAddressedChunk::deserialize(data_without_prefix, version)?;
                    Ok(Self::ContentAddressed(chunk))
                }
                ChunkType::SingleOwner => {
                    let chunk = SingleOwnerChunk::deserialize(data_without_prefix, version)?;
                    Ok(Self::SingleOwner(chunk))
                }
                ChunkType::Custom(id) => {
                    // Validate the custom chunk type ID is in the valid range
                    if !ChunkType::is_valid_custom_type(id) {
                        return Err(ChunkError::invalid_custom_type(id).into());
                    }

                    // Try to deserialize using the custom registry
                    match custom::deserialize(data_without_prefix, id, version)? {
                        Some(chunk) => Ok(Self::Custom(chunk)),
                        None => Err(ChunkError::format(format!(
                            "No deserializer found for custom chunk type {:#04x} version {}",
                            id, version
                        ))
                        .into()),
                    }
                }
            }
        } else {
            // Without a type prefix, we need to try different deserializers
            // First try ContentAddressed (most common)
            if let Ok(chunk) = ContentAddressedChunk::detect_and_deserialize(data.clone()) {
                return Ok(Self::ContentAddressed(chunk));
            }

            // Then try SingleOwner
            if let Ok(chunk) = SingleOwnerChunk::detect_and_deserialize(data.clone()) {
                return Ok(Self::SingleOwner(chunk));
            }

            // Try custom chunks
            if let Ok(Some(chunk)) = custom::detect_and_deserialize(data.clone()) {
                return Ok(Self::Custom(chunk));
            }

            Err(ChunkError::format("Could not determine chunk type").into())
        }
    }
}
