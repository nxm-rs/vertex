//! Swarm API - Core abstractions for Ethereum Swarm
//!
//! This crate defines the minimal abstractions that make a Swarm a Swarm.
//! Implementation details (libp2p, kademlia, specific storage backends) live
//! elsewhere in `net/`, `nectar/`, etc.
//!
//! # Core Concepts
//!
//! - [`SwarmReader`] - Get chunks (read-only access with bandwidth accounting)
//! - [`SwarmWriter`] - Put and get chunks (read-write access with payment)
//! - [`LocalStore`] - Local chunk persistence for full nodes
//! - [`BandwidthAccounting`] - Per-peer bandwidth tracking (pseudosettle/SWAP)
//! - [`ChunkSync`] - Sync chunks between peers
//! - [`Topology`] - Neighborhood awareness
//!
//! # Design Principles
//!
//! - Traits define *what*, implementations define *how*
//! - No libp2p concepts leak into the API
//! - Payment is configurable via associated types (can be `()` for none)
//! - Bandwidth accounting is per-peer and lock-free

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]

extern crate alloc;

mod bandwidth;
mod error;
mod store;
mod swarm;
mod sync;
mod topology;

pub use bandwidth::*;
pub use error::*;
pub use store::*;
pub use swarm::*;
pub use sync::*;
pub use topology::*;

// Re-export chunk types for convenience
pub use vertex_primitives::{
    AnyChunk, Chunk, ChunkAddress, ChunkType, ChunkTypeId, ChunkTypeSet, ContentChunk,
    OverlayAddress, PeerId, SingleOwnerChunk, StandardChunkSet, ValidatedChunk,
};
