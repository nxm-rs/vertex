//! Single-owner chunk implementation

use super::address::ChunkAddress;
use crate::error::{ChunkError, Result};
use bytes::Bytes;

/// Single-owner chunk implementation
#[derive(Debug, Clone)]
pub struct SingleOwnerChunk {
    // Fields specific to single-owner chunks
    data: Bytes,
    address: ChunkAddress,
    version: u8,
    header_size: usize,
}

impl SingleOwnerChunk {
    /// Create a new single-owner chunk
    pub fn new(data: Bytes) -> Result<Self> {
        // Implementation details
        // For now, a simple implementation that would be expanded
        if data.len() < 32 {
            return Err(ChunkError::format("Data too short").into());
        }

        // Calculate address based on content and owner
        let address = ChunkAddress::from_slice(&data[0..32])?;

        Ok(Self {
            data,
            address,
            version: 1, // Default to version 1
            header_size: 32,
        })
    }

    /// Get the chunk's address
    pub fn address(&self) -> ChunkAddress {
        self.address.clone()
    }

    /// Get the chunk's version
    pub fn version(&self) -> u8 {
        self.version
    }

    /// Get the header size
    pub fn header_size(&self) -> usize {
        self.header_size
    }

    /// Get the header
    pub fn header(&self) -> &[u8] {
        &self.data[..self.header_size]
    }

    /// Get the payload
    pub fn payload(&self) -> &[u8] {
        &self.data[self.header_size..]
    }

    /// Get the full data
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Verify the integrity of the chunk
    pub fn verify_integrity(&self) -> Result<()> {
        // Verify signature, etc.
        // Simplified implementation for now
        Ok(())
    }

    /// Deserialize with known version
    pub fn deserialize(data: Bytes, version: u8) -> Result<Self> {
        match version {
            1 => Self::deserialize_v1(data),
            _ => Err(ChunkError::format(&format!("Unsupported version: {}", version)).into()),
        }
    }

    /// Deserialize version 1
    fn deserialize_v1(data: Bytes) -> Result<Self> {
        // Version 1 specific deserialization
        Self::new(data)
    }

    /// Attempt to detect and deserialize without version info
    pub fn detect_and_deserialize(data: Bytes) -> Result<Self> {
        // Logic to detect version from data structure
        // Simplified implementation assumes version 1
        Self::deserialize_v1(data)
    }
}
