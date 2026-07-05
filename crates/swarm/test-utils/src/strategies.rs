//! Proptest value strategies for core wire types.
//!
//! Each strategy yields only values a well-formed encoder accepts, so a codec
//! test can assert an encode-then-decode round-trip over a wide input space.
//! Decode-rejection paths (short fields, out-of-range bins) stay in hand-written
//! unit tests; these strategies complement the conformance vectors, they do not
//! replace them.

use alloy_primitives::{B256, Signature};
use alloy_signer_local::PrivateKeySigner;
use libp2p::Multiaddr;
use nectar_postage::Stamp;
use nectar_primitives::{
    AnyChunk, Bin, ChunkAddress, ContentChunk, Nonce, SwarmAddress, Timestamp,
};
use proptest::prelude::*;
use vertex_swarm_api::{SwarmNodeType, SwarmSpec};
use vertex_swarm_identity::Identity;
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_primitives::{BatchId, NetworkId, StampedChunk, StorageRadius};

use crate::test_spec_isolated;

/// A uniformly random 32-byte value.
pub fn b256() -> impl Strategy<Value = B256> {
    any::<[u8; 32]>().prop_map(B256::from)
}

/// A random chunk address.
pub fn chunk_address() -> impl Strategy<Value = ChunkAddress> {
    any::<[u8; 32]>().prop_map(ChunkAddress::new)
}

/// A random overlay address.
pub fn swarm_address() -> impl Strategy<Value = SwarmAddress> {
    b256().prop_map(SwarmAddress::from)
}

/// A random postage batch id.
pub fn batch_id() -> impl Strategy<Value = BatchId> {
    b256().prop_map(BatchId::from)
}

/// A bin index across the whole `0..=MAX_PO` range.
pub fn bin() -> impl Strategy<Value = Bin> {
    (0u8..=Bin::MAX.get()).prop_map(|raw| Bin::new(raw).expect("raw is within the bin range"))
}

/// A storage radius across the whole bin range.
pub fn storage_radius() -> impl Strategy<Value = StorageRadius> {
    bin().prop_map(StorageRadius::new)
}

/// A random 32-byte nonce.
pub fn nonce() -> impl Strategy<Value = Nonce> {
    any::<[u8; 32]>().prop_map(Nonce::new)
}

/// A positive timestamp in seconds, the range a signed peer record accepts.
pub fn timestamp() -> impl Strategy<Value = Timestamp> {
    (1i64..=4_000_000_000).prop_map(Timestamp::from_seconds)
}

/// A well-formed recoverable signature: raw `r || s || v` with `v` in `{0, 1}`,
/// the parity a stamp or receipt serializes and reparses byte-for-byte.
pub fn signature() -> impl Strategy<Value = Signature> {
    (any::<[u8; 32]>(), any::<[u8; 32]>(), any::<bool>()).prop_map(|(r, s, odd)| {
        let mut raw = [0u8; 65];
        raw[..32].copy_from_slice(&r);
        raw[32..64].copy_from_slice(&s);
        raw[64] = u8::from(odd);
        Signature::from_raw(&raw).expect("a parity byte of 0 or 1 is a valid recovery id")
    })
}

/// A structurally valid postage stamp. The signature is well-formed but not
/// signed over any chunk; a codec parses the stamp bytes without verifying the
/// signature, so the round-trip holds.
pub fn stamp() -> impl Strategy<Value = Stamp> {
    (
        batch_id(),
        any::<u32>(),
        any::<u32>(),
        any::<u64>(),
        signature(),
    )
        .prop_map(|(batch, bucket, index, ts, sig)| Stamp::new(batch, bucket, index, ts, sig))
}

/// Content-chunk payload bytes, sized within a single chunk.
fn content_payload() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 1..=512)
}

/// A stamped content chunk: a content chunk paired with a structurally valid
/// stamp. The address on the wire is the chunk's own, so reconstruction on
/// decode validates it.
pub fn stamped_chunk() -> impl Strategy<Value = StampedChunk> {
    (content_payload(), stamp()).prop_map(|(data, stamp)| {
        let chunk: AnyChunk = ContentChunk::new(data)
            .expect("payload sits within chunk bounds")
            .into();
        StampedChunk::new(chunk, stamp)
    })
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

/// A validly signed peer record over the isolated test spec, so decode recovers
/// the same overlay. Pass [`swarm_peer_network_id`] to the decode boundary.
///
/// The intermediate signing identity is built inside the strategy because
/// `Identity` carries no `Debug` and so cannot itself be a strategy value.
pub fn swarm_peer() -> impl Strategy<Value = SwarmPeer> {
    let spec = test_spec_isolated();
    (any::<[u8; 32]>(), nonce(), multiaddrs(), timestamp()).prop_filter_map(
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
