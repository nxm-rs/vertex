//! Registry for custom chunk deserializers

use super::CustomChunk;
use crate::chunk::error::ChunkError;
use crate::error::Result; // Use crate-level Result
use bytes::Bytes;
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Minimum valid type ID for custom chunks
const CUSTOM_CHUNK_TYPE_MIN: u8 = 0xE0;

/// Maximum valid type ID for custom chunks
const CUSTOM_CHUNK_TYPE_MAX: u8 = 0xEF;

/// Registry for custom chunk deserializers
struct CustomChunkRegistry {
    // Map of (type_id, version) to deserializer functions
    deserializers:
        HashMap<(u8, u8), Arc<dyn Fn(Bytes) -> Result<Box<dyn CustomChunk>> + Send + Sync>>,
}

impl CustomChunkRegistry {
    /// Create a new registry
    fn new() -> Self {
        Self {
            deserializers: HashMap::new(),
        }
    }

    /// Register a deserializer for a custom chunk type and version
    fn register<F>(&mut self, type_id: u8, version: u8, deserializer: F) -> &mut Self
    where
        F: Fn(Bytes) -> Result<Box<dyn CustomChunk>> + Send + Sync + 'static,
    {
        // Only register for the allowed custom type ID range
        if type_id < CUSTOM_CHUNK_TYPE_MIN || type_id > CUSTOM_CHUNK_TYPE_MAX {
            return self;
        }

        self.deserializers
            .insert((type_id, version), Arc::new(deserializer));
        self
    }

    /// Try to deserialize custom chunk data
    fn deserialize(
        &self,
        data: Bytes,
        type_id: u8,
        version: u8,
    ) -> Result<Option<Box<dyn CustomChunk>>> {
        if let Some(deserializer) = self.deserializers.get(&(type_id, version)) {
            match deserializer(data) {
                Ok(chunk) => Ok(Some(chunk)),
                Err(e) => Err(e),
            }
        } else {
            Ok(None)
        }
    }

    /// Try to deserialize custom chunk data by trying all deserializers
    fn detect_and_deserialize(&self, data: Bytes) -> Result<Option<Box<dyn CustomChunk>>> {
        // Try each deserializer in the custom namespace (0xE0-0xEF)
        for ((type_id, _version), deserializer) in &self.deserializers {
            if *type_id >= CUSTOM_CHUNK_TYPE_MIN && *type_id <= CUSTOM_CHUNK_TYPE_MAX {
                match deserializer(data.clone()) {
                    Ok(chunk) => return Ok(Some(chunk)),
                    Err(_) => continue,
                }
            }
        }

        Ok(None)
    }

    /// Check if a type ID is in the valid custom chunk range
    fn is_valid_custom_type_id(type_id: u8) -> bool {
        type_id >= CUSTOM_CHUNK_TYPE_MIN && type_id <= CUSTOM_CHUNK_TYPE_MAX
    }
}

impl Default for CustomChunkRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// Create a global static instance directly
static GLOBAL_REGISTRY: Lazy<RwLock<CustomChunkRegistry>> =
    Lazy::new(|| RwLock::new(CustomChunkRegistry::new()));

/// Register a custom chunk deserializer
pub fn register_custom_deserializer<F>(type_id: u8, version: u8, deserializer: F) -> Result<()>
where
    F: Fn(Bytes) -> Result<Box<dyn CustomChunk>> + Send + Sync + 'static,
{
    // Validate type ID is in custom range
    if !CustomChunkRegistry::is_valid_custom_type_id(type_id) {
        return Err(ChunkError::invalid_custom_type(type_id).into());
    }

    let mut registry = GLOBAL_REGISTRY.write().map_err(|_| {
        ChunkError::registry("Failed to acquire write lock for custom chunk registry")
    })?;

    registry.register(type_id, version, deserializer);
    Ok(())
}

/// Try to deserialize custom chunk data
pub fn deserialize(data: Bytes, type_id: u8, version: u8) -> Result<Option<Box<dyn CustomChunk>>> {
    // Validate type ID is in custom range
    if !CustomChunkRegistry::is_valid_custom_type_id(type_id) {
        return Err(ChunkError::invalid_custom_type(type_id).into());
    }

    let registry = GLOBAL_REGISTRY.read().map_err(|_| {
        ChunkError::registry("Failed to acquire read lock for custom chunk registry")
    })?;

    registry.deserialize(data, type_id, version)
}

/// Try to detect and deserialize custom chunk data
pub fn detect_and_deserialize(data: Bytes) -> Result<Option<Box<dyn CustomChunk>>> {
    let registry = GLOBAL_REGISTRY.read().map_err(|_| {
        ChunkError::registry("Failed to acquire read lock for custom chunk registry")
    })?;

    registry.detect_and_deserialize(data)
}
