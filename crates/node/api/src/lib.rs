//! Node API - Component containers for Swarm nodes.
//!
//! Provides runtime containers that hold SwarmTypes instances:
//! - [`LightComponents`] - Read-only (SwarmReader)
//! - [`PublisherComponents`] - Can upload (SwarmWriter)
//! - [`FullComponents`] - Stores and syncs

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]

extern crate alloc;

mod components;
mod node;

pub use components::*;
pub use node::*;

// Re-export SwarmTypes hierarchy
pub use vertex_swarm_api::{
    BootnodeTypes, FullTypes, Identity, LightTypes, PublisherTypes,
    // Type aliases
    AccountingOf, IdentityOf, SpecOf, StorageOf, StoreOf, SyncOf, TopologyOf,
};

// Re-export swarm-api traits
pub use vertex_swarm_api::{
    AnyChunk, AvailabilityAccounting, ChunkSync, Direction, LocalStore, NoAvailabilityIncentives,
    NoPeerAvailability, PeerAvailability, SwarmError, SwarmReader, SwarmResult, SwarmWriter,
    SyncResult, Topology,
};

// Re-export common primitives
pub use async_trait::async_trait;
pub use vertex_primitives::{ChunkAddress, OverlayAddress, PeerId};
