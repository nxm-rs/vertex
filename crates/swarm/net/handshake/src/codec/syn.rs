//! Syn message encoding/decoding for handshake protocol.

use libp2p::Multiaddr;
use vertex_swarm_peer::deserialize_multiaddrs;

use crate::HandshakeError;

/// Decode a Syn proto message, returning the validated observed multiaddr.
pub(crate) fn decode_syn(
    proto: vertex_swarm_net_proto::handshake::Syn,
) -> Result<Multiaddr, HandshakeError> {
    let multiaddrs = deserialize_multiaddrs(&proto.observed_multiaddr)?;

    multiaddrs
        .into_iter()
        .next()
        .ok_or(HandshakeError::MissingField("observed_multiaddr"))
}

/// Encode an observed multiaddr into a Syn proto message.
pub(crate) fn encode_syn(observed: &Multiaddr) -> vertex_swarm_net_proto::handshake::Syn {
    vertex_swarm_net_proto::handshake::Syn {
        observed_multiaddr: observed.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_multiaddr() -> Multiaddr {
        "/ip4/127.0.0.1/tcp/1234".parse().unwrap()
    }

    #[test]
    fn test_syn_roundtrip() {
        let addr = test_multiaddr();
        let proto = encode_syn(&addr);
        let decoded = decode_syn(proto).unwrap();
        assert_eq!(addr, decoded);
    }

    #[test]
    fn test_syn_rejects_malformed_multiaddr() {
        let proto = vertex_swarm_net_proto::handshake::Syn {
            observed_multiaddr: vec![0x01, 0x02, 0x03],
        };
        let result = decode_syn(proto);
        assert!(matches!(result, Err(HandshakeError::InvalidMultiaddr(_))));
    }

    #[test]
    fn test_syn_rejects_empty_multiaddr() {
        let proto = vertex_swarm_net_proto::handshake::Syn {
            observed_multiaddr: vec![],
        };
        let result = decode_syn(proto);
        assert!(matches!(
            result,
            Err(HandshakeError::MissingField("observed_multiaddr"))
        ));
    }
}

#[cfg(test)]
pub(crate) mod proptests {
    use proptest::prelude::*;

    use super::*;

    pub(crate) fn observed_multiaddr() -> impl Strategy<Value = Multiaddr> {
        prop_oneof![
            (any::<[u8; 4]>(), any::<u16>()).prop_map(|(ip, port)| {
                format!("/ip4/{}.{}.{}.{}/tcp/{port}", ip[0], ip[1], ip[2], ip[3])
                    .parse()
                    .expect("a well-formed ip4/tcp multiaddr")
            }),
            (any::<[u8; 16]>(), any::<u16>()).prop_map(|(ip, port)| {
                format!("/ip6/{}/tcp/{port}", std::net::Ipv6Addr::from(ip))
                    .parse()
                    .expect("a well-formed ip6/tcp multiaddr")
            }),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn syn_roundtrips(observed in observed_multiaddr()) {
            let proto = encode_syn(&observed);
            let decoded = decode_syn(proto).expect("a well-formed syn decodes");
            prop_assert_eq!(observed, decoded);
        }
    }
}
