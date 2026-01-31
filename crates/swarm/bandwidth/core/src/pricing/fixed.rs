//! Fixed pricing based on proximity.

use nectar_primitives::SwarmAddress;
use nectar_primitives::ChunkAddress;
use vertex_swarm_primitives::OverlayAddress;
use vertex_swarmspec::SwarmSpec;

use super::Pricer;

/// Fixed pricing based on proximity.
///
/// Uses the formula: `price = (max_po - proximity + 1) * base_price`
#[derive(Debug, Clone)]
pub struct FixedPricer {
    base_price: u64,
    max_po: u8,
}

impl FixedPricer {
    /// Create a new fixed pricer with the given base price, deriving `max_po` from the spec.
    pub fn new(base_price: u64, spec: &impl SwarmSpec) -> Self {
        Self {
            base_price,
            max_po: spec.max_po(),
        }
    }

    /// Get the base price.
    pub fn base_price(&self) -> u64 {
        self.base_price
    }

    /// Get the max proximity order.
    pub fn max_po(&self) -> u8 {
        self.max_po
    }
}

impl Pricer for FixedPricer {
    fn price(&self, _chunk: &ChunkAddress) -> u64 {
        self.base_price
    }

    fn peer_price(&self, peer: &OverlayAddress, chunk: &ChunkAddress) -> u64 {
        let peer_addr: &SwarmAddress = peer;
        let chunk_addr: &SwarmAddress = chunk;
        let proximity = peer_addr.proximity(chunk_addr);
        let factor = (self.max_po as u64) - (proximity as u64) + 1;
        factor * self.base_price
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarmspec::init_mainnet;

    fn test_pricer(base_price: u64) -> FixedPricer {
        let spec = init_mainnet();
        FixedPricer::new(base_price, &*spec)
    }

    #[test]
    fn test_fixed_pricer_base() {
        let pricer = test_pricer(10);
        let chunk = ChunkAddress::from([0u8; 32]);
        assert_eq!(pricer.price(&chunk), 10);
    }

    #[test]
    fn test_fixed_pricer_peer_price() {
        let pricer = test_pricer(10);
        let peer = OverlayAddress::from([0u8; 32]);
        let chunk = ChunkAddress::from([0u8; 32]);
        assert_eq!(pricer.peer_price(&peer, &chunk), 10);
    }

    #[test]
    fn test_fixed_pricer_far_peer() {
        let pricer = test_pricer(10);
        let peer = OverlayAddress::from([0x00; 32]);
        let chunk_bytes = [0x80; 32];
        let chunk = ChunkAddress::from(chunk_bytes);
        assert_eq!(pricer.peer_price(&peer, &chunk), 320);
    }

    #[test]
    fn test_max_po_from_spec() {
        let spec = init_mainnet();
        let pricer = FixedPricer::new(10, &*spec);
        assert_eq!(pricer.max_po(), 31);
    }
}
