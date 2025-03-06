//! Core API traits for the Vertex Swarm node
//!
//! This crate defines the fundamental traits and interfaces that all Vertex
//! Swarm implementations must satisfy.

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]

extern crate alloc;

use async_trait::async_trait;
use vertex_primitives::{ChunkAddress, Error, Result};

/// Chunk-related traits
pub mod chunk;
pub use chunk::*;

/// Access control traits (authentication, authorization, accounting)
pub mod access;
pub use access::*;

/// Storage-related traits
pub mod storage;
pub use storage::*;

/// Network-related traits
pub mod network;
pub use network::*;

/// Node type traits
pub mod node;
pub use node::*;

/// Bandwidth management traits
pub mod bandwidth;
pub use bandwidth::*;

/// Protocol-related traits
pub mod protocol;
pub use protocol::*;

/// Common types and structures
pub mod types;
pub use types::*;

// Re-export common primitives for convenience
pub use vertex_primitives::{Address, ChunkAddress, Error, PeerId, Result, B256, U256};
