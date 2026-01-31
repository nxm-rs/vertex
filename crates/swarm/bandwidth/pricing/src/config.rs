//! Default pricing configuration.

use vertex_swarm_api::SwarmPricingConfig;

use crate::constants::DEFAULT_BASE_PRICE;

/// Default pricing configuration.
#[derive(Clone, Copy, Default)]
pub struct DefaultPricingConfig;

impl SwarmPricingConfig for DefaultPricingConfig {
    fn base_price(&self) -> u64 {
        DEFAULT_BASE_PRICE
    }
}
