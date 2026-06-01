//! Proto codec helpers for hive 2.0.0.
//!
//! Wire format mirrors bee's `pkg/hive/pb/hive.proto` — each gossiped peer
//! carries its full `BzzAddress` (multiaddrs, signature, overlay, nonce,
//! timestamp, chequebook). See [`crate::bzz`] for the canonical sign-data
//! layout.

use vertex_swarm_peer::SwarmPeer;

/// Encode SwarmPeers into a proto `Peers` message for sending.
///
/// `SwarmPeer` does not yet carry a signed timestamp / chequebook (Unit 2's
/// `BzzAddress` will plumb those through end-to-end). Until then we emit
/// zero/empty so bee can identify these records as transitional and skip
/// them in its own re-broadcast loop (bee's `hive.go:221-223` filters
/// `timestamp == 0`). Outbound interop with bee is restored in Unit 4 once
/// the handshake produces fully-signed `BzzAddress` records that we can
/// surface here.
pub(crate) fn encode_peers(peers: &[SwarmPeer]) -> vertex_swarm_net_proto::hive::Peers {
    let proto_peers = peers
        .iter()
        .map(|p| vertex_swarm_net_proto::hive::Peer {
            multiaddrs: p.serialize_multiaddrs(),
            signature: p.signature().as_bytes().to_vec(),
            overlay: p.overlay().as_slice().to_vec(),
            nonce: p.nonce().to_vec(),
            timestamp: 0,
            chequebook_address: Vec::new(),
        })
        .collect();
    vertex_swarm_net_proto::hive::Peers { peers: proto_peers }
}
