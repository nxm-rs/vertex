//! Chunk pricing configuration.

/// Configuration for chunk pricing.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmPricingConfig: Send + Sync {
    /// Base price per chunk (scaled by proximity for peer pricing).
    fn base_price(&self) -> u64;
}
