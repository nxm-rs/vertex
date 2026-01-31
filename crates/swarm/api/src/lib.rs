//! Swarm API - Core abstractions for Ethereum Swarm.

#![warn(missing_docs)]

mod components;
mod config;
mod error;
mod identity;
mod protocol;
mod providers;
mod rpc;
mod services;
mod swarm;
mod types;

pub use components::*;
pub use config::*;
pub use error::*;
pub use identity::*;
pub use protocol::*;
pub use providers::*;
pub use rpc::*;
pub use services::*;
pub use swarm::*;
pub use types::*;

// Re-export chunk types for convenience
pub use vertex_primitives::{
    AnyChunk, Chunk, ChunkAddress, ChunkType, ChunkTypeId, ChunkTypeSet, ContentChunk,
    OverlayAddress, PeerId, SingleOwnerChunk, StandardChunkSet, ValidatedChunk,
};
