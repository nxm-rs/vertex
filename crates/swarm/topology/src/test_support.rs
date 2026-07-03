//! Shared test infrastructure for topology tests.

#![allow(clippy::indexing_slicing)]

use std::sync::Arc;

use vertex_swarm_api::SwarmNodeType;
use vertex_swarm_peer_manager::{PeerManager, PeerManagerConfig};
use vertex_swarm_primitives::OverlayAddress;
use vertex_swarm_test_utils::{MockIdentity, test_overlay, test_swarm_peer};

use crate::behaviour::ConnectionRegistry;

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

/// Byte index carrying the sub-prefix that [`overlay_in_bin_with_subprefix`]
/// clusters on for `bin`: the byte just past the bin's differing bit, capped so
/// it never collides with the deep uniqueness byte.
pub(crate) fn subprefix_index(bin: u8) -> usize {
    ((bin / 8) as usize + 1).min(30)
}

/// Build an overlay at exactly proximity order `bin` to `base`, disambiguated by
/// `idx` in the deepest byte (below any small bin, so it does not move the
/// proximity order). Rng-free, so address generation stays deterministic.
pub(crate) fn overlay_in_bin(base: OverlayAddress, bin: u8, idx: u8) -> OverlayAddress {
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(base.as_slice());
    // Flip the bit at position `bin`: bits before it still match `base`, so the
    // first differing bit (the proximity order) is exactly `bin`.
    bytes[(bin / 8) as usize] ^= 0x80 >> (bin % 8);
    bytes[31] = idx;
    OverlayAddress::from(bytes)
}

/// Like [`overlay_in_bin`] but forces a shared `suffix` byte just past the bin
/// boundary. A family built with one `suffix` lands in `bin` yet shares a
/// sub-prefix, collapsing into a single sub-trie (a prefix monoculture).
pub(crate) fn overlay_in_bin_with_subprefix(
    base: OverlayAddress,
    bin: u8,
    suffix: u8,
    idx: u8,
) -> OverlayAddress {
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(base.as_slice());
    bytes[(bin / 8) as usize] ^= 0x80 >> (bin % 8);
    bytes[subprefix_index(bin)] = suffix;
    bytes[31] = idx;
    OverlayAddress::from(bytes)
}
