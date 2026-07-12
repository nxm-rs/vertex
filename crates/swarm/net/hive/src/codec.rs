//! Proto codec helpers for hive protocol (2.0.0).

use vertex_swarm_peer::SwarmPeer;

/// Encode `SwarmPeer` records into a proto `Peers` message for sending.
pub(crate) fn encode_peers(peers: &[SwarmPeer]) -> vertex_swarm_net_proto::hive::Peers {
    let proto_peers = peers
        .iter()
        .map(|p| vertex_swarm_net_proto::hive::SwarmPeer {
            multiaddrs: p.serialize_multiaddrs(),
            signature: p.signature().as_bytes().to_vec(),
            overlay: p.overlay().as_bytes().to_vec(),
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

#[cfg(test)]
mod proptests {
    use alloy_primitives::Signature;
    use proptest::prelude::*;
    use proptest_arbitrary_interop::arb;
    use vertex_swarm_peer::{
        ARBITRARY_NETWORK_ID, Nonce, OverlayAddress, SwarmPeerWire, Timestamp,
    };

    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(16))]

        // Pins the encode field mapping: every gossiped record parses back to
        // the peer it was encoded from, signature recovery included.
        #[test]
        fn encoded_peers_parse_back(peers in prop::collection::vec(arb::<SwarmPeer>(), 1..=3)) {
            let proto = encode_peers(&peers);
            prop_assert_eq!(proto.peers.len(), peers.len());
            for (p, original) in proto.peers.iter().zip(&peers) {
                let overlay =
                    OverlayAddress::from_slice(&p.overlay).expect("a 32-byte overlay");
                let nonce_bytes: [u8; 32] =
                    p.nonce.as_slice().try_into().expect("a 32-byte nonce");
                let wire = SwarmPeerWire {
                    multiaddrs_bytes: &p.multiaddrs,
                    signature: Signature::from_raw(&p.signature)
                        .expect("a 65-byte signature"),
                    overlay,
                    nonce: Nonce::new(nonce_bytes),
                    timestamp: Timestamp::from_seconds(p.timestamp),
                    chequebook_bytes: &p.chequebook_address,
                };
                let parsed = SwarmPeer::parse(wire, ARBITRARY_NETWORK_ID, None)
                    .expect("a validly signed record parses");
                prop_assert_eq!(&parsed, original);
            }
        }
    }
}
