//! Codec for hive protocol messages.

use alloy_primitives::B256;
use bytes::Bytes;
use libp2p::Multiaddr;
use vertex_net_codec::ProtocolCodec;
use vertex_net_primitives::{deserialize_underlays, serialize_underlays};

/// Codec for hive protocol messages.
pub type HiveCodec = ProtocolCodec<crate::proto::hive::Peers, Peers, HiveCodecError>;

/// Error type for hive codec operations.
#[derive(Debug, thiserror::Error)]
pub enum HiveCodecError {
    /// Protocol-level error (invalid message format, etc.)
    #[error("Protocol error: {0}")]
    Protocol(String),
    /// IO error during read/write
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    /// Invalid multiaddr
    #[error("Invalid multiaddr: {0}")]
    InvalidMultiaddr(String),
    /// Invalid overlay address length
    #[error("Invalid overlay address length: expected 32, got {0}")]
    InvalidOverlayLength(usize),
}

impl From<quick_protobuf_codec::Error> for HiveCodecError {
    fn from(error: quick_protobuf_codec::Error) -> Self {
        HiveCodecError::Protocol(error.to_string())
    }
}

/// A peer's address information for the Swarm network.
///
/// Contains everything needed to connect to and verify a peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BzzAddress {
    /// Network-level addresses (multiaddrs) for connecting to the peer.
    pub underlays: Vec<Multiaddr>,
    /// Cryptographic signature proving ownership of overlay/underlay pair.
    pub signature: Bytes,
    /// The peer's overlay address in the Kademlia topology.
    pub overlay: B256,
    /// Transaction hash nonce used in overlay address derivation.
    pub nonce: Bytes,
}

impl BzzAddress {
    /// Create a new BzzAddress.
    pub fn new(underlays: Vec<Multiaddr>, signature: Bytes, overlay: B256, nonce: Bytes) -> Self {
        Self {
            underlays,
            signature,
            overlay,
            nonce,
        }
    }
}

impl TryFrom<crate::proto::hive::BzzAddress> for BzzAddress {
    type Error = HiveCodecError;

    fn try_from(value: crate::proto::hive::BzzAddress) -> Result<Self, Self::Error> {
        let underlays = deserialize_underlays(&value.underlay)
            .map_err(|e| HiveCodecError::InvalidMultiaddr(e.to_string()))?;

        let overlay = if value.overlay.len() == 32 {
            B256::from_slice(&value.overlay)
        } else {
            return Err(HiveCodecError::InvalidOverlayLength(value.overlay.len()));
        };

        Ok(Self {
            underlays,
            signature: Bytes::from(value.signature),
            overlay,
            nonce: Bytes::from(value.nonce),
        })
    }
}

impl From<BzzAddress> for crate::proto::hive::BzzAddress {
    fn from(value: BzzAddress) -> Self {
        crate::proto::hive::BzzAddress {
            underlay: serialize_underlays(&value.underlays),
            signature: value.signature.to_vec(),
            overlay: value.overlay.to_vec(),
            nonce: value.nonce.to_vec(),
        }
    }
}

/// A message containing peer addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peers {
    /// List of peer addresses.
    pub peers: Vec<BzzAddress>,
}

impl Peers {
    /// Create a new Peers message.
    pub fn new(peers: Vec<BzzAddress>) -> Self {
        Self { peers }
    }

    /// Create an empty Peers message.
    pub fn empty() -> Self {
        Self { peers: Vec::new() }
    }

    /// Check if the message is empty.
    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// Get the number of peers.
    pub fn len(&self) -> usize {
        self.peers.len()
    }
}

impl TryFrom<crate::proto::hive::Peers> for Peers {
    type Error = HiveCodecError;

    fn try_from(value: crate::proto::hive::Peers) -> Result<Self, Self::Error> {
        let peers: Result<Vec<BzzAddress>, _> =
            value.peers.into_iter().map(BzzAddress::try_from).collect();
        Ok(Self { peers: peers? })
    }
}

impl From<Peers> for crate::proto::hive::Peers {
    fn from(value: Peers) -> Self {
        crate::proto::hive::Peers {
            peers: value.peers.into_iter().map(Into::into).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_underlay_roundtrip() {
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/1633".parse().unwrap();
        let original = BzzAddress::new(
            vec![addr],
            Bytes::from(vec![1, 2, 3]),
            B256::repeat_byte(0x42),
            Bytes::from(vec![4, 5, 6]),
        );

        let proto: crate::proto::hive::BzzAddress = original.clone().into();
        let decoded = BzzAddress::try_from(proto).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_multiple_underlays_roundtrip() {
        let addr1: Multiaddr = "/ip4/127.0.0.1/tcp/1633".parse().unwrap();
        let addr2: Multiaddr = "/ip4/192.168.1.1/udp/1634".parse().unwrap();
        let original = BzzAddress::new(
            vec![addr1, addr2],
            Bytes::from(vec![1, 2, 3]),
            B256::repeat_byte(0x42),
            Bytes::from(vec![4, 5, 6]),
        );

        let proto: crate::proto::hive::BzzAddress = original.clone().into();
        let decoded = BzzAddress::try_from(proto).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_peers_roundtrip() {
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/1633".parse().unwrap();
        let bzz = BzzAddress::new(
            vec![addr],
            Bytes::from(vec![1, 2, 3]),
            B256::repeat_byte(0x42),
            Bytes::from(vec![4, 5, 6]),
        );
        let original = Peers::new(vec![bzz]);

        let proto: crate::proto::hive::Peers = original.clone().into();
        let decoded = Peers::try_from(proto).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_empty_peers() {
        let original = Peers::empty();
        let proto: crate::proto::hive::Peers = original.clone().into();
        let decoded = Peers::try_from(proto).unwrap();
        assert_eq!(original, decoded);
        assert!(decoded.is_empty());
    }
}
