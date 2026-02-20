//! Proto codec helpers for hive protocol.

use vertex_swarm_peer::SwarmPeer;

/// Encode SwarmPeers into a proto Peers message for sending.
pub(crate) fn encode_peers(peers: &[SwarmPeer]) -> vertex_swarm_net_proto::hive::Peers {
    let proto_peers = peers
        .iter()
        .map(|p| vertex_swarm_net_proto::hive::Peer {
            multiaddrs: p.serialize_multiaddrs(),
            signature: p.signature().as_bytes().to_vec(),
            overlay: p.overlay().as_slice().to_vec(),
            nonce: p.nonce().to_vec(),
        })
        .collect();
    vertex_swarm_net_proto::hive::Peers { peers: proto_peers }
}
