//! Combined pricing and bandwidth accounting.

use vertex_swarm_api::{SwarmAccounting, SwarmClientAccounting, SwarmPricing};

/// Combined pricing and bandwidth accounting for client operations.
#[derive(Clone)]
pub struct ClientAccounting<B, P> {
    bandwidth: B,
    pricing: P,
}

impl<B, P> ClientAccounting<B, P> {
    /// Create a new client accounting instance.
    pub fn new(bandwidth: B, pricing: P) -> Self {
        Self { bandwidth, pricing }
    }

    /// Decompose into parts.
    pub fn into_parts(self) -> (B, P) {
        (self.bandwidth, self.pricing)
    }
}

impl<B, P> SwarmClientAccounting for ClientAccounting<B, P>
where
    B: SwarmAccounting + Clone + Send + Sync,
    P: SwarmPricing + Clone + Send + Sync,
{
    type Accounting = B;
    type Pricing = P;

    fn accounting(&self) -> &B {
        &self.bandwidth
    }

    fn pricing(&self) -> &P {
        &self.pricing
    }
}
