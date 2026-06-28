//! Fixed-rate pricing using `(max_po - proximity + 1) * base_price`.

use std::sync::Arc;

use nectar_primitives::{ChunkAddress, SwarmAddress};
use vertex_swarm_api::{Au, SwarmPricing};
use vertex_swarm_primitives::OverlayAddress;
use vertex_swarm_spec::SwarmSpec;

/// Prices chunks based on Kademlia proximity to peer.
#[derive(Debug)]
pub struct FixedPricer<S> {
    base_price: u64,
    spec: Arc<S>,
}

impl<S> Clone for FixedPricer<S> {
    fn clone(&self) -> Self {
        Self {
            base_price: self.base_price,
            spec: Arc::clone(&self.spec),
        }
    }
}

impl<S: SwarmSpec> FixedPricer<S> {
    /// Create a new fixed pricer.
    pub fn new(base_price: u64, spec: Arc<S>) -> Self {
        Self { base_price, spec }
    }
}

impl<S: SwarmSpec + Send + Sync + 'static> SwarmPricing for FixedPricer<S> {
    fn price(&self, _chunk: &ChunkAddress) -> Au {
        Au::from_amount(self.base_price)
    }

    fn peer_price(&self, peer: &OverlayAddress, chunk: &ChunkAddress) -> Au {
        let peer_addr: &SwarmAddress = peer;
        let chunk_addr: &SwarmAddress = chunk;
        let proximity = peer_addr.proximity(chunk_addr);
        // Proximity is capped at the canonical MAX_PO, which the default spec
        // reports as max_po, so the subtraction is non-negative; a saturating
        // subtraction keeps it that way even if a spec ever reports a lower
        // max_po than the proximity cap, never underflowing into a giant factor.
        let factor = u64::from(self.spec.max_po()).saturating_sub(u64::from(proximity.get())) + 1;
        // The proximity factor is the only bytes-to-AU multiplication on the
        // pricing path. It goes through the audited checked scaling so a large
        // base price cannot wrap into a tiny one; on overflow the price
        // saturates to the maximum, which simply fails affordability rather
        // than mispricing the chunk.
        Au::from_amount(self.base_price)
            .checked_scale(factor)
            .unwrap_or(Au::from_amount(u64::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_spec::init_mainnet;

    fn test_pricer(base_price: u64) -> FixedPricer<vertex_swarm_spec::Spec> {
        FixedPricer::new(base_price, init_mainnet())
    }

    #[test]
    fn test_base_price() {
        let pricer = test_pricer(10);
        let chunk = ChunkAddress::from([0u8; 32]);
        assert_eq!(pricer.price(&chunk), Au::from_amount(10));
    }

    #[test]
    fn test_peer_price_same_address() {
        let pricer = test_pricer(10);
        let peer = OverlayAddress::from([0u8; 32]);
        let chunk = ChunkAddress::from([0u8; 32]);
        // Same address = max proximity = factor of 1
        assert_eq!(pricer.peer_price(&peer, &chunk), Au::from_amount(10));
    }

    #[test]
    fn test_peer_price_distant() {
        let pricer = test_pricer(10);
        let peer = OverlayAddress::from([0x00; 32]);
        let chunk = ChunkAddress::from([0x80; 32]);
        // First bit differs = proximity 0 = factor of (31 - 0 + 1) = 32
        assert_eq!(pricer.peer_price(&peer, &chunk), Au::from_amount(320));
    }

    #[test]
    fn test_peer_price_saturates_on_overflow() {
        // A base price near u64::MAX times the distance factor would overflow;
        // the checked scaling saturates to the maximum rather than wrapping
        // into a tiny price.
        let pricer = test_pricer(u64::MAX);
        let peer = OverlayAddress::from([0x00; 32]);
        let chunk = ChunkAddress::from([0x80; 32]);
        assert_eq!(pricer.peer_price(&peer, &chunk), Au::from_amount(u64::MAX));
    }
}
