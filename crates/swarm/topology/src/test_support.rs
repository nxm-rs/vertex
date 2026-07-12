//! Shared test infrastructure for topology tests.

#![allow(clippy::indexing_slicing)]

use std::sync::Arc;

use vertex_swarm_api::SwarmNodeType;
use vertex_swarm_peer_manager::{PeerManager, PeerManagerConfig};
use vertex_swarm_primitives::OverlayAddress;
use vertex_swarm_test_utils::{MockIdentity, test_overlay, test_swarm_peer};

use crate::behaviour::ConnectionRegistry;
use crate::kademlia::BIT_SUFFIX_LENGTH;

pub(crate) struct TopologyTestContext {
    pub local_overlay: OverlayAddress,
    pub peer_manager: Arc<PeerManager<MockIdentity>>,
    pub connection_registry: Arc<ConnectionRegistry>,
}

impl TopologyTestContext {
    pub(crate) fn new() -> Self {
        let local = test_overlay(0);
        let identity = MockIdentity::with_overlay(local);
        let pm = PeerManager::new(&identity, PeerManagerConfig::default());
        let cr = Arc::new(ConnectionRegistry::new());
        Self {
            local_overlay: local,
            peer_manager: pm,
            connection_registry: cr,
        }
    }

    pub(crate) fn with_peers(self) -> Self {
        for n in 1..=10 {
            self.peer_manager.on_peer_connected(
                test_swarm_peer(n),
                SwarmNodeType::Storer,
                vertex_net_peer_registry::ConnectionDirection::Outbound,
                vertex_swarm_peer_manager::TrustLevel::Normal,
            );
        }
        self
    }
}

/// Build an overlay at exactly proximity order `bin` to `base`, disambiguated by
/// `idx` in the deepest byte (below any small bin, so it does not move the
/// proximity order). Rng-free, so address generation stays deterministic. Every
/// overlay lands in slot 0, so slot-aware selection degenerates to plain
/// count-based fill for populations built with this helper.
pub(crate) fn overlay_in_bin(base: OverlayAddress, bin: u8, idx: u8) -> OverlayAddress {
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(base.as_bytes());
    // Flip the bit at position `bin`: bits before it still match `base`, so the
    // first differing bit (the proximity order) is exactly `bin`.
    bytes[(bin / 8) as usize] ^= 0x80 >> (bin % 8);
    bytes[31] = idx;
    OverlayAddress::from(bytes)
}

/// Like [`overlay_in_bin`] but places the overlay in an explicit sub-prefix
/// `slot`: the [`BIT_SUFFIX_LENGTH`] bits right after the bin's differing bit
/// are set to `slot`, matching the production `slot_of` extraction. A family
/// built with one `slot` lands in `bin` yet shares a sub-trie (a monoculture);
/// distinct slots spread across the bin's sub-tries.
pub(crate) fn overlay_in_bin_with_slot(
    base: OverlayAddress,
    bin: u8,
    slot: u8,
    idx: u8,
) -> OverlayAddress {
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(base.as_bytes());
    bytes[(bin / 8) as usize] ^= 0x80 >> (bin % 8);
    for i in 0..BIT_SUFFIX_LENGTH {
        let pos = bin as usize + 1 + i as usize;
        let bit = (slot >> (BIT_SUFFIX_LENGTH - 1 - i)) & 1;
        let mask = 0x80u8 >> (pos % 8);
        if bit == 1 {
            bytes[pos / 8] |= mask;
        } else {
            bytes[pos / 8] &= !mask;
        }
    }
    bytes[31] = idx;
    OverlayAddress::from(bytes)
}
