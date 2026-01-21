//! Core primitive types for the Vertex Swarm node
//!
//! This crate provides minimal primitive types and type aliases for Vertex.
//! It intentionally keeps re-exports minimal - use `nectar_primitives` or
//! `alloy_primitives` directly for implementation details.
//!
//! # Types
//!
//! ## Core Chunk Types
//! - [`AnyChunk`] - Type-erased chunk enum
//! - [`Chunk`] - Trait for chunk types
//! - [`ChunkAddress`] - Address of a chunk (32-byte hash)
//! - [`ChunkType`], [`ChunkTypeId`] - Chunk type identification
//! - [`ContentChunk`], [`SingleOwnerChunk`] - Concrete chunk types
//!
//! ## Chunk Sets (for validation)
//! - [`ChunkTypeSet`] - Trait for specifying supported chunk types
//! - [`StandardChunkSet`], [`ContentOnlyChunkSet`] - Common configurations
//!
//! ## Validated Chunks
//! - [`ValidatedChunk`] - Type-safe wrapper proving chunk validation
//!
//! ## Address Types
//! - [`OverlayAddress`] - Swarm overlay address (32 bytes, Kademlia routing)
//! - [`PeerId`] - libp2p peer identifier (underlay, network connections)

#![cfg_attr(not(feature = "std"), no_std)]

mod validated;
pub use validated::{ValidatedChunk, ValidationError};

// Re-export only the core chunk types needed for the API
pub use nectar_primitives::{
    AnyChunk, Chunk, ChunkAddress, ChunkType, ChunkTypeId, ChunkTypeSet, ContentChunk,
    ContentOnlyChunkSet, SingleOwnerChunk, StandardChunkSet, SwarmAddress,
};

// Re-export libp2p PeerId for underlay addressing
pub use libp2p::PeerId;

// ============================================================================
// Overlay Address
// ============================================================================

/// Overlay address for Swarm routing and peer identification.
///
/// This is a 32-byte address used for:
/// - Kademlia routing (proximity/distance calculations)
/// - Bandwidth accounting (per-peer tracking)
/// - Chunk sync (peer identification)
/// - Topology (neighborhood awareness)
///
/// # Overlay vs Underlay
///
/// Swarm has two distinct address types:
///
/// - **Overlay (OverlayAddress)**: 32-byte address for Swarm-level operations.
///   Derived from the node's Ethereum address, network ID, and nonce via
///   `keccak256(eth_addr || network_id || nonce)`. Used for all Swarm protocol
///   operations (routing, accounting, sync).
///
/// - **Underlay (PeerId)**: libp2p peer identifier for actual network connections.
///   Used only in the net/ layer for establishing and managing TCP/QUIC connections.
///
/// All swarm-api traits use `OverlayAddress`. The net/ layer handles the
/// overlay ↔ underlay mapping.
pub type OverlayAddress = SwarmAddress;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_address_is_exported() {
        let _addr: ChunkAddress = ChunkAddress::default();
    }

    #[test]
    fn test_overlay_address() {
        let _addr: OverlayAddress = OverlayAddress::default();
    }
}
