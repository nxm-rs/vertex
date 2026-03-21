//! Chunk pricing based on Kademlia proximity.

use nectar_primitives::{ChunkAddress, SwarmAddress};
use vertex_swarm_api::SwarmSpec;
use vertex_swarm_primitives::OverlayAddress;

/// Calculate the price for a chunk served by a specific peer.
pub fn chunk_price<S: SwarmSpec + ?Sized>(
    spec: &S,
    peer: &OverlayAddress,
    chunk: &ChunkAddress,
) -> u64 {
    let peer_addr: &SwarmAddress = peer;
    let chunk_addr: &SwarmAddress = chunk;
    let proximity = peer_addr.proximity(chunk_addr);
    let factor = (spec.max_po() as u64) - (proximity as u64) + 1;
    factor * spec.base_price()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_spec::init_mainnet;

    #[test]
    fn test_base_price() {
        let spec = init_mainnet();
        let peer = OverlayAddress::from([0u8; 32]);
        let chunk = ChunkAddress::from([0u8; 32]);
        assert_eq!(chunk_price(&*spec, &peer, &chunk), 10_000);
    }

    #[test]
    fn test_distant_price() {
        let spec = init_mainnet();
        let peer = OverlayAddress::from([0x00; 32]);
        let chunk = ChunkAddress::from([0x80; 32]);
        assert_eq!(chunk_price(&*spec, &peer, &chunk), 320_000);
    }
}
