//! Fixed-rate pricing using `(max_po - proximity + 1) * base_price`.

use std::sync::Arc;

use nectar_primitives::{ChunkAddress, SwarmAddress};
use vertex_swarm_primitives::OverlayAddress;
use vertex_swarm_spec::SwarmSpec;

use crate::Pricer;

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

impl<S: SwarmSpec> Pricer for FixedPricer<S> {
    fn price(&self, _chunk: &ChunkAddress) -> u64 {
        self.base_price
    }

    fn peer_price(&self, peer: &OverlayAddress, chunk: &ChunkAddress) -> u64 {
        let peer_addr: &SwarmAddress = peer;
        let chunk_addr: &SwarmAddress = chunk;
        let proximity = peer_addr.proximity(chunk_addr);
        let factor = (self.spec.max_po() as u64) - (proximity as u64) + 1;
        factor * self.base_price
    }
}

impl<S: SwarmSpec + Send + Sync + 'static> vertex_swarm_api::SwarmPricing for FixedPricer<S> {
    fn price(&self, chunk: &ChunkAddress) -> u64 {
        Pricer::price(self, chunk)
    }

    fn peer_price(&self, peer: &OverlayAddress, chunk: &ChunkAddress) -> u64 {
        Pricer::peer_price(self, peer, chunk)
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
        assert_eq!(pricer.price(&chunk), 10);
    }

    #[test]
    fn test_peer_price_same_address() {
        let pricer = test_pricer(10);
        let peer = OverlayAddress::from([0u8; 32]);
        let chunk = ChunkAddress::from([0u8; 32]);
        // Same address = max proximity = factor of 1
        assert_eq!(pricer.peer_price(&peer, &chunk), 10);
    }

    #[test]
    fn test_peer_price_distant() {
        let pricer = test_pricer(10);
        let peer = OverlayAddress::from([0x00; 32]);
        let chunk = ChunkAddress::from([0x80; 32]);
        // First bit differs = proximity 0 = factor of (31 - 0 + 1) = 32
        assert_eq!(pricer.peer_price(&peer, &chunk), 320);
    }
}
