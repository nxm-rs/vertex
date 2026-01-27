//! Chunk pricing for bandwidth incentives.
//!
//! Chunks are priced based on the Kademlia distance between the requesting peer
//! and the chunk's address. Chunks that are "closer" to the peer in XOR space
//! cost more because fewer peers can serve them.
//!
//! # Formula
//!
//! ```text
//! price = (MAX_PO - proximity + 1) * base_price
//! ```

mod fixed;

pub use fixed::FixedPricer;

use vertex_primitives::{ChunkAddress, OverlayAddress};

/// Maximum proximity order for 32-byte addresses.
pub const MAX_PO: u8 = 31;

/// Trait for pricing chunks.
#[auto_impl::auto_impl(&, Arc)]
pub trait Pricer: Send + Sync {
    /// Get the base price for a chunk (not considering peer).
    fn price(&self, chunk: &ChunkAddress) -> u64;

    /// Get the price for a chunk when served by a specific peer.
    fn peer_price(&self, peer: &OverlayAddress, chunk: &ChunkAddress) -> u64;
}
