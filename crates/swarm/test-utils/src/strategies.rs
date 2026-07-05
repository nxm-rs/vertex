//! Proptest value strategies for vertex-owned wire types.
//!
//! Nectar-owned primitives (addresses, bins, nonces, stamps, chunks) draw
//! through nectar's `arbitrary` impls and generators: bridge a raw type with
//! `proptest_arbitrary_interop::arb::<T>()` and a valid-by-construction value
//! by mapping a byte strategy through `arbitrary::Unstructured` into
//! `nectar_postage::generators` or `nectar_primitives::generators`. Only the
//! vertex-owned remainder lives here. Each strategy yields only values a
//! well-formed encoder accepts; decode-rejection paths stay in hand-written
//! unit tests.

use alloy_primitives::B256;
use alloy_signer_local::PrivateKeySigner;
use libp2p::Multiaddr;
use nectar_primitives::{Bin, Nonce, Timestamp};
use proptest::prelude::*;
use proptest_arbitrary_interop::arb;
use vertex_swarm_api::{SwarmNodeType, SwarmSpec};
use vertex_swarm_identity::Identity;
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_primitives::{NetworkId, StorageRadius};

use crate::test_spec_isolated;

/// A storage radius across the whole bin range.
pub fn storage_radius() -> impl Strategy<Value = StorageRadius> {
    arb::<Bin>().prop_map(StorageRadius::new)
}

/// One or more `/ip4/../tcp/..` multiaddrs, the shape a peer record signs over.
fn multiaddrs() -> impl Strategy<Value = Vec<Multiaddr>> {
    prop::collection::vec(
        (any::<[u8; 4]>(), any::<u16>()).prop_map(|(ip, port)| {
            format!("/ip4/{}.{}.{}.{}/tcp/{port}", ip[0], ip[1], ip[2], ip[3])
                .parse()
                .expect("a well-formed ip4/tcp multiaddr")
        }),
        1..=3,
    )
}

/// A positive timestamp in seconds, the range a signed peer record accepts.
fn record_timestamp() -> impl Strategy<Value = Timestamp> {
    (1i64..=4_000_000_000).prop_map(Timestamp::from_seconds)
}

/// A validly signed peer record over the isolated test spec, so decode recovers
/// the same overlay. Pass [`swarm_peer_network_id`] to the decode boundary.
///
/// The intermediate signing identity is built inside the strategy because
/// `Identity` carries no `Debug` and so cannot itself be a strategy value.
pub fn swarm_peer() -> impl Strategy<Value = SwarmPeer> {
    let spec = test_spec_isolated();
    (
        any::<[u8; 32]>(),
        arb::<Nonce>(),
        multiaddrs(),
        record_timestamp(),
    )
        .prop_filter_map(
            "a signable peer record over a valid key",
            move |(seed, nonce, addrs, ts)| {
                let signer = PrivateKeySigner::from_bytes(&B256::from(seed)).ok()?;
                let identity = Identity::new(signer, nonce, spec.clone(), SwarmNodeType::Storer);
                SwarmPeer::sign(&identity, addrs, ts, None).ok()
            },
        )
}

/// The network id the [`swarm_peer`] strategy signs under, for the decode side.
#[must_use]
pub fn swarm_peer_network_id() -> NetworkId {
    test_spec_isolated().network_id()
}
