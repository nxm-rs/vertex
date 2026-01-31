//! Default accounting configuration (pseudosettle mode).

use vertex_swarm_api::{BandwidthMode, SwarmAccountingConfig};

use crate::constants::*;

/// Default accounting configuration with pseudosettle enabled.
#[derive(Clone, Copy, Default)]
pub struct DefaultAccountingConfig;

impl SwarmAccountingConfig for DefaultAccountingConfig {
    fn mode(&self) -> BandwidthMode {
        BandwidthMode::Pseudosettle
    }

    fn payment_threshold(&self) -> u64 {
        DEFAULT_PAYMENT_THRESHOLD
    }

    fn payment_tolerance_percent(&self) -> u64 {
        DEFAULT_PAYMENT_TOLERANCE_PERCENT
    }

    fn base_price(&self) -> u64 {
        DEFAULT_BASE_PRICE
    }

    fn refresh_rate(&self) -> u64 {
        DEFAULT_REFRESH_RATE
    }

    fn early_payment_percent(&self) -> u64 {
        DEFAULT_EARLY_PAYMENT_PERCENT
    }

    fn client_only_factor(&self) -> u64 {
        DEFAULT_CLIENT_ONLY_FACTOR
    }
}
