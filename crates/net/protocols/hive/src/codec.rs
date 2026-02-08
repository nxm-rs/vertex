//! Codec for hive protocol messages.

use vertex_net_codec::{Codec, ProtoMessage, ProtocolCodecError};
use vertex_swarm_peer::SwarmPeer;

/// Error type for hive codec operations.
pub type HiveCodecError = ProtocolCodecError<HiveProtocolError>;

/// Domain-specific errors for hive protocol.
#[derive(Debug, thiserror::Error)]
pub enum HiveProtocolError {}

/// Peers message for hive protocol.
#[derive(Debug, Clone, Default)]
pub(crate) struct Peers {
    proto: crate::proto::hive::Peers,
}

impl Peers {
    pub(crate) fn from_swarm_peers(peers: &[SwarmPeer]) -> Self {
        let proto_peers = peers
            .iter()
            .map(|p| crate::proto::hive::Peer {
                multiaddrs: p.serialize_multiaddrs(),
                signature: p.signature().as_bytes().to_vec(),
                overlay: p.overlay().as_slice().to_vec(),
                nonce: p.nonce().to_vec(),
            })
            .collect();
        Self {
            proto: crate::proto::hive::Peers { peers: proto_peers },
        }
    }

    pub(crate) fn into_proto_peers(self) -> Vec<crate::proto::hive::Peer> {
        self.proto.peers
    }

    pub(crate) fn len(&self) -> usize {
        self.proto.peers.len()
    }
}

impl ProtoMessage for Peers {
    type Proto = crate::proto::hive::Peers;
    type DecodeError = HiveCodecError;

    fn into_proto(self) -> Self::Proto {
        self.proto
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        Ok(Self { proto })
    }
}

/// Codec for hive messages.
pub(crate) type HiveCodec = Codec<Peers, HiveCodecError>;
