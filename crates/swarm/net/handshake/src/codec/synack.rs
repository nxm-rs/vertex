//! SynAck message encoding/decoding for handshake 15.0.0.

use libp2p::Multiaddr;
use vertex_swarm_peer::{BzzAddress, SwarmNodeType, Timestamp};

use super::ack::{DecodedAck, decode_ack, encode_ack};
use super::syn_msg::{decode_syn, encode_syn};
use crate::HandshakeError;
use crate::welcome::WelcomeMessage;

/// Validated contents of a decoded
/// [`SynAck`](vertex_swarm_net_proto::handshake::SynAck).
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct DecodedSynAck {
    /// Address the peer observed us at, echoed back.
    pub observed_multiaddr: Multiaddr,
    /// Inner Ack with verified peer identity.
    pub ack: DecodedAck,
}

/// Decode a SynAck proto message, returning validated components.
pub(crate) fn decode_synack(
    proto: vertex_swarm_net_proto::handshake::SynAck,
    expected_network_id: u64,
    now: Option<Timestamp>,
) -> Result<DecodedSynAck, HandshakeError> {
    let observed_multiaddr = decode_syn(proto.syn.ok_or(HandshakeError::MissingField("syn"))?)?;
    let proto_ack = proto.ack.ok_or(HandshakeError::MissingField("ack"))?;
    let ack = decode_ack(proto_ack, expected_network_id, now)?;
    Ok(DecodedSynAck {
        observed_multiaddr,
        ack,
    })
}

/// Encode components into a SynAck proto message.
pub(crate) fn encode_synack(
    observed: &Multiaddr,
    bzz: &BzzAddress,
    node_type: SwarmNodeType,
    welcome_message: &WelcomeMessage,
    network_id: u64,
) -> vertex_swarm_net_proto::handshake::SynAck {
    vertex_swarm_net_proto::handshake::SynAck {
        syn: Some(encode_syn(observed)),
        ack: Some(encode_ack(bzz, node_type, welcome_message, network_id)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_signer_local::PrivateKeySigner;
    use vertex_swarm_peer::Nonce;
    use vertex_swarm_primitives::compute_overlay;

    fn fixed_now() -> Timestamp {
        Timestamp::from_secs(1_700_000_000)
    }

    fn signed_bzz(network_id: u64) -> BzzAddress {
        let signer = PrivateKeySigner::random();
        let nonce = Nonce::from([0u8; 32]);
        let overlay = compute_overlay(&signer.address(), network_id, nonce.as_b256());
        let underlay: Vec<Multiaddr> = vec!["/ip4/192.168.1.1/tcp/5678".parse().unwrap()];
        BzzAddress::sign(
            &signer,
            underlay,
            overlay,
            network_id,
            nonce,
            fixed_now(),
            None,
        )
        .unwrap()
    }

    #[test]
    fn synack_roundtrip() {
        let observed: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();
        let bzz = signed_bzz(1);
        let welcome = WelcomeMessage::new("hi").unwrap();
        let proto = encode_synack(&observed, &bzz, SwarmNodeType::Storer, &welcome, 1);
        let decoded = decode_synack(proto, 1, Some(fixed_now())).unwrap();

        assert_eq!(decoded.observed_multiaddr, observed);
        assert_eq!(decoded.ack.bzz_address, bzz);
        assert_eq!(decoded.ack.node_type, SwarmNodeType::Storer);
        assert_eq!(decoded.ack.welcome_message, welcome);
    }

    #[test]
    fn synack_missing_syn() {
        let bzz = signed_bzz(1);
        let mut proto = encode_synack(
            &"/ip4/127.0.0.1/tcp/1234".parse().unwrap(),
            &bzz,
            SwarmNodeType::Client,
            &WelcomeMessage::empty(),
            1,
        );
        proto.syn = None;
        let err = decode_synack(proto, 1, Some(fixed_now())).unwrap_err();
        assert!(matches!(err, HandshakeError::MissingField("syn")));
    }

    #[test]
    fn synack_missing_ack() {
        let bzz = signed_bzz(1);
        let mut proto = encode_synack(
            &"/ip4/127.0.0.1/tcp/1234".parse().unwrap(),
            &bzz,
            SwarmNodeType::Client,
            &WelcomeMessage::empty(),
            1,
        );
        proto.ack = None;
        let err = decode_synack(proto, 1, Some(fixed_now())).unwrap_err();
        assert!(matches!(err, HandshakeError::MissingField("ack")));
    }
}
