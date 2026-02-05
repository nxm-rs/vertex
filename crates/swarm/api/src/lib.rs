//! Swarm API - Core abstractions for Ethereum Swarm.

#![warn(missing_docs)]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

mod components;
mod config;
mod error;
mod identity;
mod protocol;
mod providers;
mod rpc;
mod spec;
mod swarm;
mod types;

pub use components::*;
pub use config::*;
pub use error::*;
pub use identity::*;
pub use protocol::*;
pub use providers::*;
pub use rpc::*;
pub use spec::*;
pub use swarm::*;
pub use types::*;

// Re-export primitives for convenience
pub use nectar_primitives::{
    AnyChunk, Chunk, ChunkAddress, ChunkType, ChunkTypeId, ChunkTypeSet, ContentChunk,
    SingleOwnerChunk, StandardChunkSet,
};
pub use vertex_swarm_primitives::{OverlayAddress, ValidatedChunk, ValidationError};

// Re-export libp2p types used in config traits
pub use libp2p::Multiaddr;
