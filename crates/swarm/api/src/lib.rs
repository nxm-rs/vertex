//! Swarm API - Core abstractions for Ethereum Swarm
//!
//! This crate defines the minimal abstractions that make a Swarm a Swarm.
//! Implementation details (libp2p, kademlia, specific storage backends) live
//! elsewhere in `vertex-client-*`, `vertex-storer-*`, etc.
//!
//! # Type Hierarchy
//!
//! Capability levels form a hierarchy with [`BootnodeTypes`] as the root:
//!
//! - [`BootnodeTypes`] - Network participation only
//! - [`LightTypes`] - Adds retrieval with accounting
//! - [`PublisherTypes`] - Adds upload capability
//! - [`FullTypes`] - Adds local storage and sync
//!
//! # Core Concepts
//!
//! - [`SwarmReader`] - Get chunks (read-only access with availability accounting)
//! - [`SwarmWriter`] - Put and get chunks (read-write access with payment)
//! - [`LocalStore`] - Local chunk persistence for full nodes
//! - [`AvailabilityAccounting`] - Per-peer availability tracking (pseudosettle/SWAP)
//! - [`ChunkSync`] - Sync chunks between peers
//! - [`Topology`] - Neighborhood awareness
//!
//! # Protocol Integration
//!
//! - [`SwarmProtocol`] - Unified protocol for all capability levels
//! - [`SwarmServices`] - Unified services (same for all levels)
//! - [`SwarmRpcProviders`] - RPC data sources for gRPC/JSON-RPC exposure
//!
//! # Design Principles
//!
//! - Traits define *what*, implementations define *how*
//! - No libp2p concepts leak into the API
//! - Payment is configurable via associated types (can be `()` for none)
//! - Availability accounting is per-peer and lock-free
//! - Components use composition (higher levels compose lower levels)

#![warn(missing_docs)]

mod components;
mod config;
mod error;
mod protocol;
mod providers;
mod rpc;
mod services;
mod swarm;
mod types;

pub use components::*;
pub use config::*;
pub use error::*;
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
