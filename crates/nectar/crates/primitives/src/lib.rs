//! Core primitives for a decentralized storage system.
//!
//! This crate provides the foundational types and traits for working with
//! chunks and storage-related access control in a decentralized storage network.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![warn(missing_docs)]

// Re-export dependencies that are part of our public API
pub use bytes;
pub use nectar_access_control;

// Core modules
pub mod bmt;
pub mod chunk;
pub mod error;
pub mod storage;

// Re-exports of primary types
pub use bmt::{error::DigestError, BMTHasher};
pub use chunk::{error::ChunkError, ChunkAddress, ChunkType, CustomChunk};
pub use error::{Error, Result};
pub use storage::{error::StorageError, PostageStamp, StorageController, StorageCredential};

/// Constants used throughout the crate
pub mod constants {
    // Re-export BMT constants
    pub use crate::bmt::constants::*;

    /// Size of a chunk address in bytes (same as hash size)
    pub const ADDRESS_SIZE: usize = HASH_SIZE;

    /// Maximum size of a chunk in bytes
    pub const MAX_CHUNK_SIZE: usize = 4096;

    /// Size of a batch ID in bytes
    pub const BATCH_ID_SIZE: usize = 32;

    /// Size of an owner address in bytes
    pub const OWNER_SIZE: usize = 20;
}
