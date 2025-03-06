//! Core traits and interfaces for the Vertex Swarm node
//!
//! This crate defines the fundamental traits that make up the Vertex Swarm API.
//! It provides the contract for component interactions without specifying implementations.

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::String, vec::Vec};

pub use async_trait::async_trait;
pub use vertex_primitives::{self, ChunkAddress, Error, PeerId, Result};

/// Chunk-related traits and types
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

/// Node-related traits
pub mod node;
pub use node::*;

/// Bandwidth management traits
pub mod bandwidth;
pub use bandwidth::*;

/// Protocol-related traits
pub mod protocol;
pub use protocol::*;
