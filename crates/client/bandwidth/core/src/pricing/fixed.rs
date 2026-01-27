//! Fixed pricing based on proximity.

use nectar_primitives::SwarmAddress;
use vertex_primitives::{ChunkAddress, OverlayAddress};

use super::{MAX_PO, Pricer};

/// Default base price per chunk in accounting units.
pub const DEFAULT_BASE_PRICE: u64 = 10_000;

/// Fixed pricing based on proximity.
///
/// Uses the formula: `price = (MAX_PO - proximity + 1) * base_price`
#[derive(Debug, Clone)]
pub struct FixedPricer {
    base_price: u64,
}

impl FixedPricer {
    /// Create a new fixed pricer with the given base price.
    pub fn new(base_price: u64) -> Self {
        Self { base_price }
    }

    /// Get the base price.
    pub fn base_price(&self) -> u64 {
        self.base_price
    }
}

impl Default for FixedPricer {
    fn default() -> Self {
        Self::new(DEFAULT_BASE_PRICE)
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
        let factor = (MAX_PO as u64) - (proximity as u64) + 1;
        factor * self.base_price
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fixed_pricer_base() {
        let pricer = FixedPricer::new(10);
        let chunk = ChunkAddress::from([0u8; 32]);
        assert_eq!(pricer.price(&chunk), 10);
    }

    #[test]
    fn test_fixed_pricer_peer_price() {
        let pricer = FixedPricer::new(10);
        let peer = OverlayAddress::from([0u8; 32]);
        let chunk = ChunkAddress::from([0u8; 32]);
        assert_eq!(pricer.peer_price(&peer, &chunk), 10);
    }

    #[test]
    fn test_fixed_pricer_far_peer() {
        let pricer = FixedPricer::new(10);
        let peer = OverlayAddress::from([0x00; 32]);
        let chunk_bytes = [0x80; 32];
        let chunk = ChunkAddress::from(chunk_bytes);
        assert_eq!(pricer.peer_price(&peer, &chunk), 320);
    }
}
