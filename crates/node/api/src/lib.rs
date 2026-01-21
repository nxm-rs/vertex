//! Node API - Composition and lifecycle for Swarm nodes
//!
//! This crate builds on [`vertex_node_types`] to provide:
//!
//! - Component containers ([`NodeComponents`], [`PublisherComponents`], [`FullNodeComponents`])
//! - Type-safe swarm access with enforced matching between NodeTypes and Swarm
//!
//! # Architecture
//!
//! ```text
//! node-types (what types?)        node-api (composition + enforcement)
//! ─────────────────────────       ────────────────────────────────────
//! NodeTypes (read-only)      ──►  NodeComponents<N, S: SwarmReader>
//!   + DataAvailability             where S::Accounting = N::DataAvailability
//!
//! PublisherNodeTypes         ──►  PublisherComponents<N, S: SwarmWriter>
//!   + StoragePayment               where S::Accounting = N::DataAvailability
//!                                        S::Payment = N::StoragePayment
//!
//! FullNodeTypes              ──►  FullNodeComponents<N, S: SwarmWriter>
//!   + Store, Sync                  (same bounds + store/sync)
//! ```
//!
//! # Usage
//!
//! ```ignore
//! // Create components (types must match!)
//! let components = FullNodeComponents::new(swarm, topology, store, sync);
//!
//! // Use the Swarm client
//! components.swarm().put(chunk, &proof).await?;
//!
//! // Access bandwidth accounting
//! let peer_acct = components.accounting().for_peer(peer_id);
//! ```

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]

extern crate alloc;

mod components;
mod node;

pub use components::*;
pub use node::*;

// Re-export node-types for convenience
pub use vertex_node_types::{
    // Builder
    AnyNodeTypes,
    // Type aliases
    ChunkSetOf,
    DataAvailabilityOf,
    // Core traits (hierarchy)
    FullNodeTypes,
    NodeTypes,
    NodeTypesWithSpec,
    PublisherNodeTypes,
    SpecOf,
    StoragePaymentOf,
    StoreOf,
    SyncOf,
    TopologyOf,
};

// Re-export swarm-api traits for convenience
pub use vertex_swarm_api::{
    AnyChunk, BandwidthAccounting, ChunkSync, Direction, LocalStore, NoBandwidthIncentives,
    NoPeerBandwidth, PeerBandwidth, SwarmError, SwarmReader, SwarmResult, SwarmWriter, SyncResult,
    Topology,
};

// Re-export common primitives
pub use async_trait::async_trait;
pub use vertex_primitives::{ChunkAddress, OverlayAddress, PeerId};
