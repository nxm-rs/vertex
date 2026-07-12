//! Proptest value strategies for vertex-owned wire types.
//!
//! Thin wrappers over the workspace `arbitrary` layer via
//! `proptest_arbitrary_interop::arb`, so proptest suites and fuzz targets
//! drive the same valid-by-construction construction code. Nectar-owned
//! primitives (addresses, bins, nonces, stamps, chunks) bridge the same way
//! through nectar's `arbitrary` impls and generators.

use proptest::prelude::*;
use proptest_arbitrary_interop::arb;
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_primitives::{NetworkId, StorageRadius};

/// A storage radius across the whole bin range.
pub fn storage_radius() -> impl Strategy<Value = StorageRadius> {
    arb::<StorageRadius>()
}

/// A validly signed peer record, so decode recovers the same overlay. Pass
/// [`swarm_peer_network_id`] to the decode boundary.
pub fn swarm_peer() -> impl Strategy<Value = SwarmPeer> {
    arb::<SwarmPeer>()
}

/// The network id the [`swarm_peer`] strategy signs under, for the decode side.
#[must_use]
pub fn swarm_peer_network_id() -> NetworkId {
    vertex_swarm_peer::ARBITRARY_NETWORK_ID
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arbitrary_network_id_matches_the_isolated_test_spec() {
        assert_eq!(swarm_peer_network_id().get(), crate::spec::TEST_NETWORK_ID);
    }
}
