//! Chunk-related primitive types

#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
use core::fmt::Debug;

/// Possible chunk types in the Swarm network
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChunkType {
    /// Standard content-addressed chunk (default)
    ContentAddressed,
    /// Encrypted chunk with custom addressing
    Encrypted,
    /// Manifest chunk for organizing other chunks
    Manifest,
    /// Custom chunk type with identifier
    Custom(u8),
}

impl ChunkType {
    /// Convert chunk type to byte identifier
    pub const fn to_byte(&self) -> u8 {
        match self {
            Self::ContentAddressed => 0,
            Self::Encrypted => 1,
            Self::Manifest => 2,
            Self::Custom(id) => *id,
        }
    }

    /// Create chunk type from byte identifier
    pub const fn from_byte(byte: u8) -> Self {
        match byte {
            0 => Self::ContentAddressed,
            1 => Self::Encrypted,
            2 => Self::Manifest,
            id => Self::Custom(id),
        }
    }
}

/// A peer identifier in the network
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PeerId(pub Vec<u8>);

impl PeerId {
    /// Creates a new PeerId from raw bytes
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Returns the underlying bytes
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Debug for PeerId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "PeerId({})", hex::encode(&self.0))
    }
}

/// Protocol ID type
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ProtocolId(
    #[cfg(not(feature = "std"))] pub alloc::string::String,
    #[cfg(feature = "std")] pub String,
);

impl ProtocolId {
    /// Creates a new ProtocolId
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Returns the protocol ID as a string
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Direction of data transfer
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Outgoing data
    Outgoing,
    /// Incoming data
    Incoming,
}
