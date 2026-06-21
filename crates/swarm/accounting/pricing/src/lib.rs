//! Chunk pricing for bandwidth accounting.
//!
//! Price formula: `(max_po - proximity + 1) * base_price`
//!
//! Distant chunks cost more; nearby chunks cost less.

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "cli")]
pub mod args;
mod config;
mod constants;
mod fixed;

#[cfg(feature = "cli")]
pub use args::FixedPricingArgs;
pub use config::FixedPricingConfig;
pub use fixed::FixedPricer;

use nectar_primitives::ChunkAddress;
use vertex_swarm_api::Au;
use vertex_swarm_primitives::OverlayAddress;

/// Trait for pricing chunks.
#[auto_impl::auto_impl(&, Arc)]
pub trait Pricer: Send + Sync {
    /// Get the base price for a chunk in AU (not considering peer).
    fn price(&self, chunk: &ChunkAddress) -> Au;

    /// Get the price for a chunk in AU when served by a specific peer.
    fn peer_price(&self, peer: &OverlayAddress, chunk: &ChunkAddress) -> Au;
}

/// No-op pricer for nodes that don't participate in pricing (e.g., bootnodes).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoPricer;

impl Pricer for NoPricer {
    fn price(&self, _chunk: &ChunkAddress) -> Au {
        Au::ZERO
    }

    fn peer_price(&self, _peer: &OverlayAddress, _chunk: &ChunkAddress) -> Au {
        Au::ZERO
    }
}

impl vertex_swarm_api::SwarmPricing for NoPricer {
    fn price(&self, chunk: &ChunkAddress) -> Au {
        Pricer::price(self, chunk)
    }

    fn peer_price(&self, peer: &OverlayAddress, chunk: &ChunkAddress) -> Au {
        Pricer::peer_price(self, peer, chunk)
    }
}
