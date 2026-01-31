//! Chunk pricing based on Kademlia proximity.

use nectar_primitives::ChunkAddress;
use vertex_swarm_primitives::OverlayAddress;

/// Configuration for chunk pricing.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmPricingConfig: Send + Sync {
    /// Base price per chunk (scaled by proximity for peer pricing).
    fn base_price(&self) -> u64;
}

/// Chunk pricing strategy.
///
/// Calculates prices in Accounting Units (AU) based on peer proximity.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmPricing: Clone + Send + Sync {
    /// Base price for a chunk (ignoring peer proximity).
    fn price(&self, chunk: &ChunkAddress) -> u64;

    /// Price for a chunk served by a specific peer (proximity-adjusted).
    fn peer_price(&self, peer: &OverlayAddress, chunk: &ChunkAddress) -> u64;
}
