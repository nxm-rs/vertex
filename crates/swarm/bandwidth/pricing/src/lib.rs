//! Chunk pricing for bandwidth accounting.
//!
//! Price formula: `(max_po - proximity + 1) * base_price`
//!
//! Distant chunks cost more; nearby chunks cost less.

#![cfg_attr(not(feature = "std"), no_std)]

mod fixed;

pub use fixed::FixedPricer;

use nectar_primitives::ChunkAddress;
use vertex_swarm_primitives::OverlayAddress;

/// Trait for pricing chunks.
#[auto_impl::auto_impl(&, Arc)]
pub trait Pricer: Send + Sync {
    /// Get the base price for a chunk (not considering peer).
    fn price(&self, chunk: &ChunkAddress) -> u64;

    /// Get the price for a chunk when served by a specific peer.
    fn peer_price(&self, peer: &OverlayAddress, chunk: &ChunkAddress) -> u64;
}

/// No-op pricer for nodes that don't participate in pricing (e.g., bootnodes).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoPricer;

impl Pricer for NoPricer {
    fn price(&self, _chunk: &ChunkAddress) -> u64 {
        0
    }

    fn peer_price(&self, _peer: &OverlayAddress, _chunk: &ChunkAddress) -> u64 {
        0
    }
}
