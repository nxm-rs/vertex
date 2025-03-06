//! Custom chunk types and registry

mod registry;

pub use registry::{deserialize, detect_and_deserialize, register_custom_deserializer};

use super::address::ChunkAddress;
use crate::error::Result; // Use crate-level Result
use dyn_clone::DynClone;

/// Trait for custom chunk types (used with ChunkData::Custom variant)
pub trait CustomChunk: DynClone + Send + Sync + 'static {
    /// Get the chunk's address/hash
    fn address(&self) -> ChunkAddress;

    /// Get the chunk type identifier (should be in the 0xE0-0xEF range)
    fn type_id(&self) -> u8;

    /// Get the version of the chunk format
    fn version(&self) -> u8;

    /// Get the chunk's header based on its type
    fn header(&self) -> &[u8];

    /// Get the chunk's payload (data excluding header)
    fn payload(&self) -> &[u8];

    /// Get the complete raw data
    fn data(&self) -> &[u8];

    /// Verify the integrity of the chunk
    fn verify_integrity(&self) -> Result<()>;
}

// Enable cloning for CustomChunk trait objects
dyn_clone::clone_trait_object!(CustomChunk);
