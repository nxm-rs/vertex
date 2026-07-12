//! Fuzz-facing surface behind the `arbitrary` feature: the gossip batch
//! validation entry point and valid-by-construction generators. Dev-only;
//! never enabled by a shipped artefact cone.

use arbitrary::Unstructured;

use crate::cache::PeerCache;

pub use crate::error::ValidationFailure;
pub use vertex_swarm_net_proto::hive as proto;
pub use vertex_swarm_peer::{ARBITRARY_NETWORK_ID, OverlayAddress, SwarmPeer};
pub use vertex_swarm_primitives::NetworkId;

/// Frame cap the framed hive codec enforces on decode.
pub const MAX_MESSAGE_SIZE: usize = crate::protocol::MAX_MESSAGE_SIZE;

/// Session-scoped validation cache, as the inbound reader holds one.
#[derive(Default)]
pub struct ValidationCache(PeerCache);

/// Validate a decoded batch through the exact conversion path the inbound
/// reader runs after the rate-limit charge.
///
/// Returns (valid_peers, valid_count, invalid_count).
pub fn validate_peers(
    raw_peers: Vec<proto::SwarmPeer>,
    network_id: NetworkId,
    local_overlay: &OverlayAddress,
    cache: &ValidationCache,
) -> (Vec<SwarmPeer>, usize, usize) {
    crate::protocol::validate_batch(raw_peers, network_id, local_overlay, &cache.0)
}

/// Encode records with the outbound field mapping.
pub fn encode_peers(peers: &[SwarmPeer]) -> proto::Peers {
    crate::codec::encode_peers(peers)
}

/// A really signed record under [`ARBITRARY_NETWORK_ID`] whose multiaddrs all
/// carry a `/p2p/` component, so full gossip validation passes.
pub fn arbitrary_routable_peer(u: &mut Unstructured<'_>) -> arbitrary::Result<SwarmPeer> {
    let addrs = (0..u.int_in_range(1..=3u8)?)
        .map(|_| vertex_swarm_peer::arbitrary_multiaddr_with_peer_id(u))
        .collect::<arbitrary::Result<Vec<_>>>()?;
    SwarmPeer::arbitrary_signed_with_addrs(u, ARBITRARY_NETWORK_ID, addrs)
}

/// A wire peer record: adversarial field content, or a really signed routable
/// record so the recovery success path and the cache stay covered.
pub fn arbitrary_wire_peer(u: &mut Unstructured<'_>) -> arbitrary::Result<proto::SwarmPeer> {
    if u.arbitrary()? {
        let peer = arbitrary_routable_peer(u)?;
        let mut batch = crate::codec::encode_peers(std::slice::from_ref(&peer));
        return Ok(batch.peers.pop().unwrap_or_default());
    }
    Ok(proto::SwarmPeer {
        multiaddrs: vertex_swarm_peer::fuzz::arbitrary_multiaddrs_bytes(u)?,
        signature: u.arbitrary()?,
        overlay: u.arbitrary()?,
        nonce: u.arbitrary()?,
        timestamp: u.arbitrary()?,
        chequebook_address: u.arbitrary()?,
    })
}

/// A wire batch of up to `max` records mixing signed and adversarial arms.
pub fn arbitrary_wire_peers(
    u: &mut Unstructured<'_>,
    max: usize,
) -> arbitrary::Result<proto::Peers> {
    let count = u.int_in_range(0..=max)?;
    let peers = (0..count)
        .map(|_| arbitrary_wire_peer(u))
        .collect::<arbitrary::Result<Vec<_>>>()?;
    Ok(proto::Peers { peers })
}
