//! Codec for hive protocol messages.

use vertex_net_codec::{Codec, ProtoMessage, ProtocolCodecError};
use vertex_swarm_peer::SwarmPeer;

/// Error type for hive codec operations.
pub type HiveCodecError = ProtocolCodecError<HiveProtocolError>;

/// Domain-specific errors for hive protocol.
#[derive(Debug, thiserror::Error)]
pub enum HiveProtocolError {
    /// Peer validation failed
    #[error("Invalid peer: {0}")]
    InvalidPeer(String),
}

/// Peers message for hive protocol.
#[derive(Debug, Clone, Default)]
pub struct Peers {
    proto: crate::proto::hive::Peers,
}

impl Peers {
    /// Create from SwarmPeers (for outbound).
    pub fn from_swarm_peers(peers: &[SwarmPeer]) -> Self {
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

    /// Consume and return raw proto peers (for inbound validation).
    pub fn into_proto_peers(self) -> Vec<crate::proto::hive::Peer> {
        self.proto.peers
    }

    pub fn is_empty(&self) -> bool {
        self.proto.peers.is_empty()
    }

    pub fn len(&self) -> usize {
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
pub type HiveCodec = Codec<Peers, HiveCodecError>;
