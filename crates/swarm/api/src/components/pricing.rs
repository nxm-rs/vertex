//! Chunk pricing based on Kademlia proximity.

use std::sync::Arc;

use nectar_primitives::ChunkAddress;
use vertex_swarm_primitives::OverlayAddress;

use crate::SwarmSpec;

/// Configuration that holds a pricing strategy.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmPricingConfig: Send + Sync {
    /// The pricing configuration type.
    type Pricing: Default + Clone + Send + Sync;

    /// Get the pricing configuration.
    fn pricing(&self) -> &Self::Pricing;
}

/// Builder that creates a pricer from configuration and spec.
pub trait SwarmPricingBuilder<S: SwarmSpec>: Clone + Send + Sync {
    /// The pricer type produced by this builder.
    type Pricer: SwarmPricing + Clone + Send + Sync + 'static;

    /// Build a pricer from configuration and spec.
    fn build_pricer(&self, spec: Arc<S>) -> Self::Pricer;
}

/// Chunk pricing strategy.
///
/// Calculates prices in Accounting Units (AU) based on peer proximity.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmPricing: Send + Sync {
    /// Base price for a chunk (ignoring peer proximity).
    fn price(&self, chunk: &ChunkAddress) -> u64;

    /// Price for a chunk served by a specific peer (proximity-adjusted).
    fn peer_price(&self, peer: &OverlayAddress, chunk: &ChunkAddress) -> u64;
}
