//! Proto codec helpers for hive protocol (2.0.0).

use vertex_swarm_peer::SwarmPeer;

/// Encode `SwarmPeer` records into a proto `Peers` message for sending.
pub(crate) fn encode_peers(peers: &[SwarmPeer]) -> vertex_swarm_net_proto::hive::Peers {
    let proto_peers = peers
        .iter()
        .map(|p| vertex_swarm_net_proto::hive::SwarmPeer {
            multiaddrs: p.serialize_multiaddrs(),
            signature: p.signature().as_bytes().to_vec(),
            overlay: p.overlay().as_slice().to_vec(),
            nonce: p.nonce().as_slice().to_vec(),
            timestamp: p.timestamp().get(),
            chequebook_address: p
                .chequebook()
                .map(|a| a.as_slice().to_vec())
                .unwrap_or_default(),
        })
        .collect();
    vertex_swarm_net_proto::hive::Peers { peers: proto_peers }
}
